//! PSI (Pressure Stall Information) memory trigger.
//!
//! Replaces "scan every N seconds, blind" with "wake up when the kernel
//! tells us there is real memory pressure". The kernel exposes per-resource
//! stall accounting under `/proc/pressure/{cpu,memory,io}`. By writing a
//! `<some|full> <stall_us> <window_us>` line to one of these files in
//! `O_RDWR` mode, userspace registers a trigger; subsequent `poll(POLLPRI)`
//! on that fd blocks until the kernel sees the configured stall accumulate
//! within the rolling window.
//!
//! References:
//!   - <https://www.kernel.org/doc/html/latest/accounting/psi.html>
//!   - kernel/sched/psi.c
//!
//! Permissions: registering a trigger requires `CAP_SYS_RESOURCE`. When
//! that cap is missing the open succeeds but the write returns EPERM,
//! and the daemon falls back to timer-only mode (see main.rs). Reading
//! the file works for any user.

use anyhow::{bail, Context, Result};
use libc::{poll, pollfd, POLLERR, POLLPRI};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

const PSI_MEMORY_PATH: &str = "/proc/pressure/memory";

/// Owns the registered PSI trigger fd. Drop closes the fd, which
/// automatically unregisters the trigger in the kernel.
#[derive(Debug)]
pub struct PsiTrigger {
    fd: OwnedFd,
}

impl PsiTrigger {
    /// Register a "some" memory pressure trigger.
    ///
    /// `stall_us` is the cumulative stall threshold in microseconds; once
    /// processes spend that much time waiting on memory inside any
    /// rolling `window_us` interval, `poll_event` will return Ok(true).
    /// Sane defaults: `(150_000, 1_000_000)` ⇒ 150ms in 1s.
    ///
    /// Returns Err if the file does not exist (kernel without
    /// CONFIG_PSI), if the open fails, or if the write fails (typically
    /// EPERM without `CAP_SYS_RESOURCE`).
    pub fn open_memory(stall_us: u64, window_us: u64) -> Result<Self> {
        if !Path::new(PSI_MEMORY_PATH).exists() {
            bail!(
                "{} missing — kernel was built without CONFIG_PSI?",
                PSI_MEMORY_PATH,
            );
        }

        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(PSI_MEMORY_PATH)
            .with_context(|| format!("opening {} O_RDWR", PSI_MEMORY_PATH))?;

        let trigger = format!("some {} {}", stall_us, window_us);
        file.write_all(trigger.as_bytes()).with_context(|| {
            format!("writing PSI trigger {:?} (needs CAP_SYS_RESOURCE)", trigger,)
        })?;

        Ok(Self {
            fd: OwnedFd::from(file),
        })
    }

    /// Block on POLLPRI for up to `timeout`. Designed to be called from
    /// a dedicated blocking thread because `poll(2)` has no async
    /// counterpart in tokio that exposes POLLPRI.
    ///
    /// Returns:
    ///   - `Ok(true)`  trigger fired
    ///   - `Ok(false)` timed out without a fire — caller should loop
    ///   - `Err(_)`    poll(2) failed or kernel reported POLLERR
    pub fn poll_event(&self, timeout: Duration) -> Result<bool> {
        let mut fds = [pollfd {
            fd: self.fd.as_raw_fd(),
            events: POLLPRI,
            revents: 0,
        }];

        // poll(2) takes timeout in milliseconds, -1 = infinite.
        let timeout_ms: i32 = timeout.as_millis().try_into().unwrap_or(i32::MAX);

        let n = unsafe { poll(fds.as_mut_ptr(), 1, timeout_ms) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                // EINTR — caller's polling loop will simply retry
                return Ok(false);
            }
            bail!("poll PSI fd: {}", err);
        }
        if n == 0 {
            return Ok(false);
        }

        let revents = fds[0].revents;
        if revents & POLLERR != 0 {
            bail!("PSI trigger reported POLLERR (was the trigger force-unregistered?)",);
        }
        Ok(revents & POLLPRI != 0)
    }
}

/// Minimal PSI snapshot returned by [`read_memory`] for diagnostics.
/// All averages are percentages of wall time over 10s / 60s / 300s.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemoryPressure {
    pub some_avg10: f32,
    pub some_avg60: f32,
    pub some_avg300: f32,
    pub full_avg10: f32,
    pub full_avg60: f32,
    pub full_avg300: f32,
}

/// One-shot read of /proc/pressure/memory. Useful for snapshotting the
/// current pressure state into telemetry without registering a trigger.
/// Requires no special caps.
pub fn read_memory() -> Result<MemoryPressure> {
    let raw = std::fs::read_to_string(PSI_MEMORY_PATH)
        .with_context(|| format!("reading {}", PSI_MEMORY_PATH))?;
    parse_memory(&raw)
}

/// Pure parser, factored out for unit tests.
pub fn parse_memory(raw: &str) -> Result<MemoryPressure> {
    let mut p = MemoryPressure::default();
    for line in raw.lines() {
        let mut parts = line.split_whitespace();
        let kind = match parts.next() {
            Some(k) => k,
            None => continue,
        };
        let mut a10 = 0.0f32;
        let mut a60 = 0.0f32;
        let mut a300 = 0.0f32;
        for kv in parts {
            let mut split = kv.splitn(2, '=');
            let key = split.next().unwrap_or("");
            let val: f32 = split.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            match key {
                "avg10" => a10 = val,
                "avg60" => a60 = val,
                "avg300" => a300 = val,
                _ => {}
            }
        }
        match kind {
            "some" => {
                p.some_avg10 = a10;
                p.some_avg60 = a60;
                p.some_avg300 = a300;
            }
            "full" => {
                p.full_avg10 = a10;
                p.full_avg60 = a60;
                p.full_avg300 = a300;
            }
            _ => {}
        }
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_memory_canonical_layout() {
        let raw = "\
some avg10=0.12 avg60=0.05 avg300=0.01 total=12345
full avg10=0.00 avg60=0.00 avg300=0.00 total=0
";
        let p = parse_memory(raw).unwrap();
        assert!((p.some_avg10 - 0.12).abs() < 1e-3);
        assert!((p.some_avg60 - 0.05).abs() < 1e-3);
        assert!((p.some_avg300 - 0.01).abs() < 1e-3);
        assert_eq!(p.full_avg10, 0.0);
        assert_eq!(p.full_avg300, 0.0);
    }

    #[test]
    fn parse_memory_handles_empty_or_partial() {
        // Should not panic and should return zeros where data missing
        let p = parse_memory("").unwrap();
        assert_eq!(p, MemoryPressure::default());
        let p = parse_memory("some avg10=garbage").unwrap();
        assert_eq!(p.some_avg10, 0.0);
    }

    #[test]
    fn parse_memory_handles_only_some_line() {
        let p = parse_memory("some avg10=1.5 avg60=2.5 avg300=3.5 total=99").unwrap();
        assert!((p.some_avg10 - 1.5).abs() < 1e-3);
        assert_eq!(p.full_avg10, 0.0);
    }

    /// Smoke test: trigger registration is permission-gated, so we can't
    /// rely on it in CI — but parsing the value works on any modern
    /// kernel without elevation. If PSI is missing, skip silently.
    #[test]
    fn read_memory_smoke_or_skip() {
        if !Path::new(PSI_MEMORY_PATH).exists() {
            eprintln!("PSI not enabled in kernel — skipping smoke test");
            return;
        }
        let p = read_memory().expect("PSI file readable on every modern kernel");
        // Sanity: averages should be non-negative finite floats
        for v in [
            p.some_avg10,
            p.some_avg60,
            p.some_avg300,
            p.full_avg10,
            p.full_avg60,
            p.full_avg300,
        ] {
            assert!(v.is_finite() && v >= 0.0, "got bogus value {}", v);
        }
    }
}
