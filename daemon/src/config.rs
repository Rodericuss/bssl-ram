use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// Seconds a tab must be idle before being compressed
    pub idle_threshold_secs: u64,

    /// Minimum RSS (MiB) a process must have to be worth compressing
    pub min_rss_mib: u64,

    /// WebSocket port the daemon listens on
    pub ws_port: u16,

    /// Whether to actually call process_madvise (false = dry run, just log)
    pub dry_run: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_threshold_secs: 60,
            min_rss_mib: 50,
            ws_port: 7878,
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
