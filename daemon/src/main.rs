mod compressor;
mod config;
mod psi;
mod scanner;
mod state;
mod telemetry;
mod zram;

use anyhow::Result;
use compressor::{compress_pid, read_proc_stats, rss_mib};
use config::Config;
use scanner::scan_targets;
use state::{CpuTracker, ProcSnapshot};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use telemetry::Stats;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Notify;
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

    // PSI memory pressure trigger. When the kernel reports that processes
    // collectively spent > psi_stall_threshold_us waiting on memory inside
    // any psi_window_us window, we want to scan immediately instead of
    // waiting for the next timer tick. The blocking thread translates
    // poll(POLLPRI) events into Notify wake-ups for the async loop.
    //
    // If registration fails (kernel without CONFIG_PSI, missing
    // CAP_SYS_RESOURCE, ...) we log once and stay timer-only — every
    // other code path keeps working.
    let psi_notify = Arc::new(Notify::new());
    let shutdown = Arc::new(AtomicBool::new(false));
    let psi_active = if config.psi_enabled {
        match psi::PsiTrigger::open_memory(config.psi_stall_threshold_us, config.psi_window_us) {
            Ok(trigger) => {
                info!(
                    stall_us = config.psi_stall_threshold_us,
                    window_us = config.psi_window_us,
                    "PSI memory pressure trigger registered — daemon is event-driven"
                );
                spawn_psi_thread(trigger, psi_notify.clone(), shutdown.clone());
                true
            }
            Err(e) => {
                warn!(
                    err = %e,
                    "PSI trigger unavailable — falling back to timer-only mode",
                );
                false
            }
        }
    } else {
        info!("PSI disabled by config — timer-only mode");
        false
    };

    let mut tracker = CpuTracker::new();
    let stats = Stats::default();
    let mut cycle: u64 = 0;
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(config.scan_interval_secs)) => {
                cycle += 1;
                scan_cycle(&config, &mut tracker, &stats, cycle, ScanTrigger::Timer);

                if config.telemetry_interval_cycles > 0
                    && cycle.is_multiple_of(config.telemetry_interval_cycles)
                {
                    stats.emit();
                }
            }
            _ = psi_notify.notified() => {
                // Notified() blocks forever when nobody is sending — so
                // when PSI is off (psi_active = false) this arm simply
                // never fires and the timer arm above carries the loop.
                cycle += 1;
                stats.inc(&stats.psi_events);
                // Snapshot current pressure for the log line so the operator
                // can see *how bad* it was when the scan fired.
                if let Ok(p) = psi::read_memory() {
                    info!(
                        some_avg10 = p.some_avg10,
                        some_avg60 = p.some_avg60,
                        full_avg10 = p.full_avg10,
                        "PSI pressure event — running adaptive scan",
                    );
                }
                scan_cycle(&config, &mut tracker, &stats, cycle, ScanTrigger::PsiPressure);
            }
            _ = tokio::signal::ctrl_c() => {
                info!("SIGINT received — shutting down gracefully");
                shutdown.store(true, Ordering::Relaxed);
                stats.emit();
                break;
            }
            _ = sigterm.recv() => {
                info!("SIGTERM received — shutting down gracefully");
                shutdown.store(true, Ordering::Relaxed);
                stats.emit();
                break;
            }
        }
    }

    // Surface the chosen mode in the final shutdown line so a journalctl
    // grep can quickly confirm which path the daemon used in this session.
    info!(psi_active, "daemon stopping");
    Ok(())
}

/// Why this scan is happening — surfaces in the per-cycle log so a
/// `grep trigger=psi-pressure` shows exactly which scans were reactive.
#[derive(Debug, Clone, Copy)]
enum ScanTrigger {
    Timer,
    PsiPressure,
}

impl ScanTrigger {
    fn as_str(self) -> &'static str {
        match self {
            ScanTrigger::Timer => "timer",
            ScanTrigger::PsiPressure => "psi-pressure",
        }
    }
}

/// Move PSI's blocking poll loop off the tokio runtime. tokio's AsyncFd
/// can register POLLIN/POLLOUT but does not expose POLLPRI, which is
/// what PSI triggers fire on — so a dedicated OS thread is the simplest
/// correct path. The 5s timeout bounds shutdown latency without having
/// to wire up an extra eventfd.
fn spawn_psi_thread(trigger: psi::PsiTrigger, notify: Arc<Notify>, shutdown: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("bssl-ram-psi".into())
        .spawn(move || {
            while !shutdown.load(Ordering::Relaxed) {
                match trigger.poll_event(Duration::from_secs(5)) {
                    Ok(true) => notify.notify_one(),
                    Ok(false) => continue,
                    Err(e) => {
                        warn!(err = %e, "PSI poll error — backing off 1s");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            }
            debug!("PSI thread exiting (shutdown signalled)");
        })
        .expect("spawn PSI poll thread");
}

/// One pass of the idle-detection + compression pipeline.
///
/// Wrapped in a `cycle` span so every line emitted from inside carries
/// `cycle=N` and the per-cycle scan duration. Each per-PID decision is
/// logged as a single structured event with `action=<verb>` and
/// `reason=<why>` fields — grep `action=compress` to see only what the
/// daemon *actually did*.
fn scan_cycle(
    config: &Config,
    tracker: &mut CpuTracker,
    stats: &Stats,
    cycle: u64,
    trigger: ScanTrigger,
) {
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
        trigger = trigger.as_str(),
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

    // Seen-this-cycle set feeds the end-of-cycle GC that drops tracker
    // state for PIDs we no longer observe at all (never coming back,
    // not PID-reused either — just gone).
    let mut seen_pids: HashSet<u32> = HashSet::with_capacity(targets.len());

    for tab in &targets {
        let pid = tab.pid;
        let profile = tab.profile.as_str();
        seen_pids.insert(pid);

        let (starttime, utime, stime) = match read_proc_stats(pid) {
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

        let snap = ProcSnapshot {
            pid,
            starttime,
            utime,
            stime,
        };
        let is_idle = tracker.update(
            snap,
            config.cpu_delta_threshold,
            config.wakeup_delta_threshold,
        );
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
            Ok(outcome) if outcome.is_real_success() => {
                // Only NOW set the anti-recompression flag. If we set it on
                // every Ok we'd silence retries after a partial/total
                // syscall failure (ENOSYS, EPERM, ESRCH on non-x86_64
                // builds, …) and the daemon would look healthy while
                // doing nothing.
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
                    "page-out done"
                );
            }
            Ok(outcome) if outcome.was_dry_run => {
                // Dry-run does NOT mutate the tracker — the next cycle
                // should re-evaluate (and re-log) the same PID instead of
                // silently being marked "already compressed" and skipped.
                info!(
                    pid,
                    profile,
                    action = "would-compress",
                    reason = "dry-run",
                    rss_mib = rss,
                    regions = outcome.regions,
                    "dry-run: would page out (no syscall issued)"
                );
            }
            Ok(outcome) => {
                // Real call but partial or total batch failure. Don't
                // mark compressed — leave the door open for the next
                // idle cycle to retry once whatever the kernel objected
                // to is gone.
                stats.inc(&stats.errors);
                warn!(
                    pid,
                    profile,
                    action = "compress-incomplete",
                    reason = "partial-batch-failure",
                    rss_mib = rss,
                    regions = outcome.regions,
                    bytes_advised_mib = outcome.bytes_advised / 1024 / 1024,
                    batches = outcome.batches,
                    batches_failed = outcome.batches_failed,
                    "process_madvise rejected one or more batches — not marking as compressed",
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

    // End-of-cycle GC: drop tracker state for every PID we did NOT see
    // in this scan. The PID-reuse guard in CpuTracker::update() already
    // protects PIDs that come back; this sweeps orphans that just
    // vanished (Firefox kills a content proc, Chrome tab navigates
    // away, Electron app quits, etc.).
    let pre = tracker.tracked_pids();
    tracker.retain_only(&seen_pids);
    let post = tracker.tracked_pids();
    if pre > post {
        debug!(
            dropped = pre - post,
            remaining = post,
            "tracker gc: dropped stale PIDs"
        );
    }

    // Quick per-cycle counters, useful when grep-ing live output instead
    // of waiting for the next telemetry snapshot.
    debug!(
        scanned = stats.scans.load(Ordering::Relaxed),
        compressions = stats.compressions.load(Ordering::Relaxed),
        bytes_mib = stats.bytes_paged_out.load(Ordering::Relaxed) / 1024 / 1024,
        tracked_pids = post,
        "running totals"
    );
}
