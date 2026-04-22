mod compressor;
mod config;
mod scanner;
mod state;
mod zram;

use anyhow::Result;
use compressor::{compress_pid, read_cpu_ticks, rss_mib};
use config::Config;
use scanner::scan_firefox_tabs;
use state::{CpuTracker, ProcSnapshot};
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tracing::{debug, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::load()?;
    info!(
        "bssl-ram starting — scan every {}s, idle threshold: {} cycles, cpu delta: {} ticks",
        config.scan_interval_secs,
        config.idle_cycles_threshold,
        config.cpu_delta_threshold,
    );

    if config.dry_run {
        info!("DRY RUN mode — no actual compression will happen");
    }

    zram::ensure_zram_swap(&config)?;

    let mut tracker = CpuTracker::new();
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(config.scan_interval_secs)) => {
                scan_cycle(&config, &mut tracker);
            }
            _ = tokio::signal::ctrl_c() => {
                info!("SIGINT received — shutting down gracefully");
                break;
            }
            _ = sigterm.recv() => {
                info!("SIGTERM received — shutting down gracefully");
                break;
            }
        }
    }

    Ok(())
}

/// One pass of the idle-detection + compression pipeline.
/// Pulled out of the main loop so the tokio::select! above can preempt
/// between cycles on a signal without leaving a scan half-done.
fn scan_cycle(config: &Config, tracker: &mut CpuTracker) {
    let tabs = scan_firefox_tabs();
    info!("found {} Firefox tab processes", tabs.len());

    for tab in &tabs {
        let pid = tab.pid;

        let (utime, stime) = match read_cpu_ticks(pid) {
            Some(t) => t,
            None => {
                // Process exited between scan and now
                tracker.remove(pid);
                continue;
            }
        };

        let snap = ProcSnapshot { pid, utime, stime };
        let is_idle = tracker.update(snap, config.cpu_delta_threshold);
        let cycles = tracker.idle_cycles(pid);

        if !is_idle {
            continue;
        }

        if cycles < config.idle_cycles_threshold {
            info!(
                "pid {} idle for {}/{} cycles — waiting",
                pid, cycles, config.idle_cycles_threshold
            );
            continue;
        }

        // Already compressed during this idle period — skip. The tracker
        // clears this flag the moment the process shows CPU activity again,
        // so a new idle period will be eligible.
        if tracker.is_compressed(pid) {
            debug!("pid {} already compressed — skipping", pid);
            continue;
        }

        let rss = rss_mib(pid);
        if rss < config.min_rss_mib {
            info!(
                "pid {} idle but RSS {}MiB < threshold {}MiB — skipping",
                pid, rss, config.min_rss_mib
            );
            continue;
        }

        match compress_pid(pid, config.dry_run) {
            Ok(()) => tracker.mark_compressed(pid),
            Err(e) => warn!("failed to compress pid {}: {}", pid, e),
        }
    }
}
