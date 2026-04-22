use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// How many consecutive idle cycles before compressing a process.
    /// Each cycle is scan_interval_secs long.
    /// Default: 3 cycles × 10s = 30s of idle before compressing.
    pub idle_cycles_threshold: u32,

    /// Seconds between each scan of /proc
    pub scan_interval_secs: u64,

    /// CPU ticks delta below this value is considered idle.
    /// Linux scheduler ticks at 100Hz — 1 tick = 10ms of CPU.
    /// Default: 2 ticks = 20ms of CPU per interval = essentially idle.
    pub cpu_delta_threshold: u64,

    /// Minimum RSS (MiB) a process must have to be worth compressing.
    /// Skip tiny processes to avoid wasting syscall overhead.
    pub min_rss_mib: u64,

    /// Log what would happen without actually compressing anything.
    pub dry_run: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_cycles_threshold: 3,
            scan_interval_secs: 10,
            cpu_delta_threshold: 2,
            min_rss_mib: 50,
            dry_run: false,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Path::new("/etc/bssl-ram/config.toml");
        if path.exists() {
            let raw = std::fs::read_to_string(path)?;
            Ok(toml::from_str(&raw)?)
        } else {
            Ok(Self::default())
        }
    }
}
