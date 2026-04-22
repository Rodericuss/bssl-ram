use crate::scanner::{default_profiles, BrowserProfile};
use anyhow::Result;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
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

    /// CPU ticks delta above this value is treated as a real user
    /// wakeup and clears the anti-recompression flag for that PID. Set
    /// well above `cpu_delta_threshold` so the small bursts that
    /// browsers fire while idle (GC, service-worker pulses, internal
    /// timers) don't masquerade as activity and trigger a recompression
    /// of pages that are already in zram.
    /// Default: 50 ticks = 500ms of CPU per interval.
    pub wakeup_delta_threshold: u64,

    /// Minimum RSS (MiB) a process must have to be worth compressing.
    /// Skip tiny processes to avoid wasting syscall overhead.
    pub min_rss_mib: u64,

    /// Log what would happen without actually compressing anything.
    pub dry_run: bool,

    /// Emit a cumulative-stats snapshot every N scan cycles. Default 60
    /// (≈10 min at the default 10s interval). Set to 0 to disable.
    #[serde(default = "default_telemetry_interval")]
    pub telemetry_interval_cycles: u64,

    /// Browser/app profiles used by the scanner. Each profile is a
    /// declarative cmdline-match rule. Defaults to a built-in set covering
    /// Firefox-family + Chromium-family + Electron apps. Users can replace
    /// or extend the list in `/etc/bssl-ram/config.toml`:
    ///
    /// ```toml
    /// [[profiles]]
    /// name = "my-app"
    /// binary_substring_any = ["myapp"]
    /// arg_required_all = ["--worker"]
    /// ```
    #[serde(default = "default_profiles")]
    pub profiles: Vec<BrowserProfile>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            idle_cycles_threshold: 3,
            scan_interval_secs: 10,
            cpu_delta_threshold: 2,
            wakeup_delta_threshold: 50,
            min_rss_mib: 50,
            dry_run: false,
            telemetry_interval_cycles: default_telemetry_interval(),
            profiles: default_profiles(),
        }
    }
}

fn default_telemetry_interval() -> u64 {
    60
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_config_uses_defaults_for_missing_fields() {
        // Council-flagged regression: previously a config containing only
        // some of the fields blew up at startup with `missing field
        // idle_cycles_threshold`, contradicting the README which promises
        // every field is optional. Struct-level `#[serde(default)]` plus
        // `impl Default` make this work: the present fields override the
        // defaults, the missing ones inherit.
        let parsed: Config = toml::from_str("dry_run = true\n").expect("partial config must parse");
        let defaults = Config::default();

        assert!(parsed.dry_run);
        assert_eq!(parsed.idle_cycles_threshold, defaults.idle_cycles_threshold);
        assert_eq!(parsed.scan_interval_secs, defaults.scan_interval_secs);
        assert_eq!(parsed.cpu_delta_threshold, defaults.cpu_delta_threshold);
        assert_eq!(parsed.min_rss_mib, defaults.min_rss_mib);
        assert_eq!(
            parsed.telemetry_interval_cycles,
            defaults.telemetry_interval_cycles,
        );
        assert_eq!(parsed.profiles.len(), defaults.profiles.len());
    }

    #[test]
    fn empty_config_parses_to_defaults() {
        let parsed: Config = toml::from_str("").expect("empty config must parse");
        let defaults = Config::default();
        assert_eq!(parsed.idle_cycles_threshold, defaults.idle_cycles_threshold);
        assert_eq!(parsed.scan_interval_secs, defaults.scan_interval_secs);
        assert!(!parsed.dry_run);
    }
}
