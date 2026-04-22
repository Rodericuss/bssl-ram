mod compressor;
mod config;
mod scanner;
mod state;
mod telemetry;
mod zram;

use anyhow::Result;
use compressor::{compress_pid, read_cpu_ticks, rss_mib};
use config::Config;
use scanner::scan_targets;
use state::{CpuTracker, ProcSnapshot};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use telemetry::Stats;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, info, info_span, warn};

#[tokio::main]
async fn main() -> Result<()> {
    telemetry::init();

    let config = Config::load()?;
    info!(
        scan_interval_secs = config.scan_interval_secs,
        idle_cycles_threshold = config.idle_cycles_threshold,
        cpu_delta_threshold = config.cpu_delta_threshold,
        min_rss_mib = config.min_rss_mib,
        telemetry_interval_cycles = config.telemetry_interval_cycles,
        dry_run = config.dry_run,
        "bssl-ram starting"
    );
    info!(
        active_profiles = ?config
            .profiles
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        "scanner profiles loaded"
    );

    if config.dry_run {
        info!("DRY RUN mode — no actual compression will happen");
    }

    zram::ensure_zram_swap(&config)?;

    let mut tracker = CpuTracker::new();
    let stats = Stats::default();
    let mut cycle: u64 = 0;
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(config.scan_interval_secs)) => {
                cycle += 1;
                scan_cycle(&config, &mut tracker, &stats, cycle);

                if config.telemetry_interval_cycles > 0
                    && cycle.is_multiple_of(config.telemetry_interval_cycles)
                {
                    stats.emit();
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("SIGINT received — shutting down gracefully");
                stats.emit();
                break;
            }
            _ = sigterm.recv() => {
                info!("SIGTERM received — shutting down gracefully");
                stats.emit();
                break;
            }
        }
    }

    Ok(())
}

/// One pass of the idle-detection + compression pipeline.
///
/// Wrapped in a `cycle` span so every line emitted from inside carries
/// `cycle=N` and the per-cycle scan duration. Each per-PID decision is
/// logged as a single structured event with `action=<verb>` and
/// `reason=<why>` fields — grep `action=compress` to see only what the
/// daemon *actually did*.
fn scan_cycle(config: &Config, tracker: &mut CpuTracker, stats: &Stats, cycle: u64) {
    let started = Instant::now();
    let targets = scan_targets(&config.profiles);

    let mut by_profile: HashMap<&str, usize> = HashMap::new();
    for t in &targets {
        *by_profile.entry(t.profile.as_str()).or_insert(0) += 1;
    }
    let mut summary: Vec<String> = by_profile
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect();
    summary.sort();

    let span = info_span!(
        "cycle",
        n = cycle,
        targets = targets.len(),
        breakdown = %if summary.is_empty() { "—".into() } else { summary.join(" ") },
    );
    let _enter = span.enter();

    stats.inc(&stats.scans);
    stats.add(&stats.targets_seen, targets.len() as u64);

    info!(
        scan_ms = started.elapsed().as_millis() as u64,
        "scan complete"
    );

    for tab in &targets {
        let pid = tab.pid;
        let profile = tab.profile.as_str();

        let (utime, stime) = match read_cpu_ticks(pid) {
            Some(t) => t,
            None => {
                tracker.remove(pid);
                debug!(
                    pid,
                    profile,
                    action = "skip",
                    reason = "exited",
                    "process exited between scan and decision"
                );
                continue;
            }
        };

        let snap = ProcSnapshot { pid, utime, stime };
        let is_idle = tracker.update(snap, config.cpu_delta_threshold);
        let cycles = tracker.idle_cycles(pid);

        if !is_idle {
            stats.inc(&stats.skips_active);
            debug!(
                pid,
                profile,
                action = "skip",
                reason = "active",
                idle_cycles = cycles,
                "process active this cycle"
            );
            continue;
        }

        if cycles < config.idle_cycles_threshold {
            stats.inc(&stats.skips_warmup);
            info!(
                pid,
                profile,
                action = "wait",
                reason = "warmup",
                idle_cycles = cycles,
                idle_threshold = config.idle_cycles_threshold,
                "idle but not yet at threshold"
            );
            continue;
        }

        if tracker.is_compressed(pid) {
            stats.inc(&stats.skips_already_compressed);
            debug!(
                pid,
                profile,
                action = "skip",
                reason = "already-compressed",
                idle_cycles = cycles,
                "skipping; pages already in zram and process has not woken since"
            );
            continue;
        }

        let rss = rss_mib(pid);
        if rss < config.min_rss_mib {
            stats.inc(&stats.skips_low_rss);
            info!(
                pid,
                profile,
                action = "skip",
                reason = "low-rss",
                rss_mib = rss,
                min_rss_mib = config.min_rss_mib,
                "rss below floor"
            );
            continue;
        }

        match compress_pid(pid, config.dry_run) {
            Ok(outcome) => {
                tracker.mark_compressed(pid);
                stats.inc(&stats.compressions);
                stats.add(&stats.bytes_paged_out, outcome.bytes_advised);
                stats.add(
                    &stats.bytes_skipped_by_kernel,
                    outcome.bytes_skipped_by_kernel,
                );
                info!(
                    pid,
                    profile,
                    action = "compress",
                    reason = "idle",
                    rss_mib = rss,
                    regions = outcome.regions,
                    bytes_advised_mib = outcome.bytes_advised / 1024 / 1024,
                    bytes_skipped_mib = outcome.bytes_skipped_by_kernel / 1024 / 1024,
                    batches = outcome.batches,
                    dry_run = config.dry_run,
                    "page-out done"
                );
            }
            Err(e) => {
                stats.inc(&stats.errors);
                warn!(
                    pid,
                    profile,
                    action = "error",
                    reason = "compress-failed",
                    err = %e,
                    "process_madvise pipeline failed"
                );
            }
        }
    }

    // Quick per-cycle counters, useful when grep-ing live output instead
    // of waiting for the next telemetry snapshot.
    debug!(
        scanned = stats.scans.load(Ordering::Relaxed),
        compressions = stats.compressions.load(Ordering::Relaxed),
        bytes_mib = stats.bytes_paged_out.load(Ordering::Relaxed) / 1024 / 1024,
        "running totals"
    );
}
