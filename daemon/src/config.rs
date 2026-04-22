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

    /// Register a PSI (Pressure Stall Information) memory trigger so the
    /// daemon also wakes up *immediately* when the kernel reports real
    /// memory pressure, instead of being purely timer-driven. With PSI on,
    /// `scan_interval_secs` becomes a safety-net cap rather than the only
    /// wake source — when the system is comfortable the daemon idles
    /// and burns essentially zero CPU; when pressure spikes it reacts in
    /// the same cycle.
    ///
    /// Requires CAP_SYS_RESOURCE to register the trigger (the systemd
    /// unit grants it). If registration fails for any reason (kernel
    /// without CONFIG_PSI, missing cap, etc.) the daemon logs a warning
    /// and silently falls back to timer-only mode — the rest of the
    /// pipeline is unchanged.
    #[serde(default = "default_psi_enabled")]
    pub psi_enabled: bool,

    /// Cumulative stall threshold in microseconds within
    /// `psi_window_us`. Defaults to 150 000 (150ms). Lower values fire
    /// earlier (more reactive, more wake-ups under load); higher values
    /// only react to severe pressure.
    #[serde(default = "default_psi_stall_us")]
    pub psi_stall_threshold_us: u64,

    /// Rolling window in microseconds over which `psi_stall_threshold_us`
    /// is measured. Defaults to 1 000 000 (1s) — the documented sane
    /// minimum for PSI triggers.
    #[serde(default = "default_psi_window_us")]
    pub psi_window_us: u64,

    /// Load the eBPF cpu_tracker program at startup and read per-task
    /// CPU runtime from a kernel hash map instead of opening
    /// `/proc/PID/stat` every cycle. Requires CAP_BPF + CAP_PERFMON
    /// (both granted by the systemd unit) and a kernel with raw_tp
    /// support (≥ 5.8). Falls back to /proc/PID/stat polling if the
    /// load fails for any reason.
    ///
    /// Off by default in v0.3.0-alpha — flip to true to opt in.
    #[serde(default = "default_bpf_enabled")]
    pub enable_bpf_cpu_tracker: bool,

    /// Drift correction interval for the cn_proc-driven process table.
    /// Every N scan cycles, the table is reconciled against a fresh
    /// /proc walk so any events the kernel dropped (under fork storm,
    /// SO_RCVBUF saturation, …) get caught up. Set to 0 to disable
    /// (dangerous on long uptimes — drift accumulates). Default 10 ⇒
    /// reseed every 10 cycles ≈ every 100s at the default scan_interval.
    #[serde(default = "default_cn_proc_reseed")]
    pub cn_proc_reseed_every_n_cycles: u64,

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
            psi_enabled: default_psi_enabled(),
            psi_stall_threshold_us: default_psi_stall_us(),
            psi_window_us: default_psi_window_us(),
            cn_proc_reseed_every_n_cycles: default_cn_proc_reseed(),
            enable_bpf_cpu_tracker: default_bpf_enabled(),
            profiles: default_profiles(),
        }
    }
}

fn default_telemetry_interval() -> u64 {
    60
}

fn default_psi_enabled() -> bool {
    true
}

fn default_psi_stall_us() -> u64 {
    150_000
}

fn default_psi_window_us() -> u64 {
    1_000_000
}

fn default_cn_proc_reseed() -> u64 {
    10
}

fn default_bpf_enabled() -> bool {
    false
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
