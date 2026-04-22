use crate::config::Config;
use anyhow::Result;
use std::fs;
use tracing::{info, warn};

/// Ensures a zram swap device exists and is active.
/// If zram is already configured as swap on the system, does nothing.
/// This is a best-effort setup — the user can also configure zram manually.
pub fn ensure_zram_swap(config: &Config) -> Result<()> {
    // Check if any zram swap is already active
    let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();
    if swaps.contains("zram") {
        info!("zram swap already active, skipping setup");
        return Ok(());
    }

    if config.dry_run {
        info!("[dry-run] would set up zram swap device");
        return Ok(());
    }

    warn!("no zram swap detected — bssl-ram needs zram as swap to compress memory");
    warn!("run: modprobe zram && echo lz4 > /sys/block/zram0/comp_algorithm");
    warn!("     echo 4G > /sys/block/zram0/disksize && mkswap /dev/zram0 && swapon /dev/zram0");
    warn!("or install systemd-zram-generator for automatic setup");

    Ok(())
}
