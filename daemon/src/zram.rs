use crate::config::Config;
use anyhow::Result;
use std::fs;
use tracing::{info, warn};

/// Checks if zram swap is active and warns if not.
/// bssl-ram pages memory out to swap — if swap is not zram,
/// pages will go to disk which defeats the purpose.
pub fn ensure_zram_swap(_config: &Config) -> Result<()> {
    let swaps = fs::read_to_string("/proc/swaps").unwrap_or_default();

    if swaps.contains("zram") {
        info!("zram swap detected — ready");
        return Ok(());
    }

    if swaps.lines().count() > 1 {
        warn!("swap is active but NOT zram — pages will go to disk, not compressed RAM");
        warn!("for best results, set up zram: https://wiki.archlinux.org/title/Zram");
    } else {
        warn!("no swap detected — MADV_PAGEOUT will have no effect");
        warn!("set up zram swap:");
        warn!("  sudo modprobe zram");
        warn!("  echo lz4 | sudo tee /sys/block/zram0/comp_algorithm");
        warn!("  echo 4G  | sudo tee /sys/block/zram0/disksize");
        warn!("  sudo mkswap /dev/zram0 && sudo swapon /dev/zram0");
        warn!("or install zram-generator: sudo pacman -S zram-generator");
    }

    Ok(())
}
