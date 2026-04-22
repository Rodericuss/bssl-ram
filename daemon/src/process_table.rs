//! In-memory process table maintained from cn_proc events.
//!
//! Replaces the per-cycle `/proc` walk: the table is seeded once at
//! startup by walking `/proc` and applying [`scanner::match_profile`],
//! then kept in sync by a dedicated thread that reads
//! [`proc_connector::ProcEvent`] off the netlink socket. Each scan
//! cycle reads the current table snapshot via [`ProcessTable::live_targets`].
//!
//! What we save:
//!   - Discovery cost: 1 readdir on `/proc` + 1 cmdline read per pid → 0
//!   - Per-cycle work shrinks from O(total_pids) to O(matched_targets)
//!
//! What still costs syscalls per cycle (until eBPF lands): one
//! `/proc/PID/stat` read per matched target, for the CPU delta.

use crate::proc_connector::{ProcConnector, ProcEvent};
use crate::scanner::{match_profile, parse_cmdline, BrowserProfile, TargetProcess};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tracing::{debug, info, warn};

/// What we know about a process in the table. `profile_name` is `None`
/// for unmatched processes — we still track them so an EXEC event can
/// flip them to matched without re-walking /proc.
#[derive(Debug, Clone)]
struct ProcessRecord {
    profile_name: Option<String>,
}

/// Thread-safe shared table. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct ProcessTable {
    inner: Arc<RwLock<HashMap<u32, ProcessRecord>>>,
    profiles: Arc<Vec<BrowserProfile>>,
    shutdown: Arc<AtomicBool>,
}

impl ProcessTable {
    /// Build the table by:
    ///   1. Opening the cn_proc netlink socket (fails fast if unavailable
    ///      so the caller can fall back to /proc-walk mode).
    ///   2. Seeding the table by walking `/proc` once.
    ///   3. Spawning a blocking OS thread that drains the netlink socket
    ///      and applies events to the table.
    ///
    /// The thread terminates when `shutdown` flips to `true`.
    pub fn spawn(profiles: Vec<BrowserProfile>) -> Result<Self> {
        let connector = ProcConnector::open()?;

        let table = Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            profiles: Arc::new(profiles),
            shutdown: Arc::new(AtomicBool::new(false)),
        };

        table.seed_from_proc();
        info!(
            seeded = table.tracked(),
            "process table seeded from /proc walk"
        );

        let thread_clone = table.clone();
        std::thread::Builder::new()
            .name("bssl-ram-cn_proc".into())
            .spawn(move || thread_clone.run_event_loop(connector))
            .expect("spawn cn_proc reader thread");

        Ok(table)
    }

    /// Walk /proc once at startup so the table is non-empty when the
    /// first scan_cycle runs. Idempotent — re-applying the same
    /// (pid, profile) pair just overwrites the entry.
    fn seed_from_proc(&self) {
        let dir = match fs::read_dir("/proc") {
            Ok(d) => d,
            Err(e) => {
                warn!(err = %e, "process table: cannot read /proc for seed");
                return;
            }
        };
        let mut table = self.inner.write().unwrap();
        for entry in dir.flatten() {
            let name = entry.file_name();
            let pid: u32 = match name.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue,
            };
            if let Some(record) = self.classify_pid(pid) {
                table.insert(pid, record);
            }
        }
    }

    /// Read the cmdline for `pid` and run it through every profile.
    /// Returns `Some` only when the process matched (we don't bother
    /// tracking unmatched PIDs — saves memory in the table).
    fn classify_pid(&self, pid: u32) -> Option<ProcessRecord> {
        let raw = fs::read(format!("/proc/{}/cmdline", pid)).ok()?;
        if raw.is_empty() {
            // Process is freshly forked but hasn't exec'd yet, OR it
            // exited between EVENT and read. Skip; we'll see EXEC later.
            return None;
        }
        let args = parse_cmdline(&raw);
        for profile in self.profiles.iter() {
            if match_profile(&args, profile) {
                return Some(ProcessRecord {
                    profile_name: Some(profile.name.clone()),
                });
            }
        }
        None
    }

    /// Long-lived loop: blocks on `recv_events`, applies each to the
    /// table. Exits when shutdown is requested. The recv has no
    /// timeout, so on shutdown the thread will linger until the socket
    /// closes (process exit) — acceptable for a daemon.
    fn run_event_loop(&self, connector: ProcConnector) {
        let mut buf = [0u8; 4096];
        info!("cn_proc reader thread up — table will refresh on every fork/exec/exit");

        while !self.shutdown.load(Ordering::Relaxed) {
            match connector.recv_events(&mut buf) {
                Ok(events) => {
                    if events.is_empty() {
                        continue;
                    }
                    for ev in events {
                        self.apply_event(ev);
                    }
                }
                Err(e) => {
                    warn!(err = %e, "cn_proc recv error — backing off 1s");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
        debug!("cn_proc reader thread exiting (shutdown signalled)");
    }

    fn apply_event(&self, event: ProcEvent) {
        match event {
            ProcEvent::Fork { child, .. } => {
                // A new task. We can't classify yet because cmdline is
                // typically empty until exec(). Wait for EXEC.
                debug!(pid = child, "cn_proc: fork");
            }
            ProcEvent::Exec { pid } => {
                if let Some(record) = self.classify_pid(pid) {
                    let mut table = self.inner.write().unwrap();
                    let added = !table.contains_key(&pid);
                    table.insert(pid, record.clone());
                    if added {
                        info!(
                            pid,
                            profile = %record.profile_name.as_deref().unwrap_or("?"),
                            "cn_proc: target appeared (exec)",
                        );
                    }
                } else {
                    // Either cmdline isn't readable yet, or it matched
                    // no profile. Drop any stale record (the same PID
                    // could have been a target before this exec).
                    let mut table = self.inner.write().unwrap();
                    if table.remove(&pid).is_some() {
                        info!(pid, "cn_proc: target gone (exec replaced argv)");
                    }
                }
            }
            ProcEvent::Exit { pid } => {
                let mut table = self.inner.write().unwrap();
                if table.remove(&pid).is_some() {
                    info!(pid, "cn_proc: target gone (exit)");
                }
            }
        }
    }

    /// Snapshot of currently-known matched targets. Cheap clone of
    /// strings — caller can iterate freely.
    pub fn live_targets(&self) -> Vec<TargetProcess> {
        let table = self.inner.read().unwrap();
        table
            .iter()
            .filter_map(|(pid, rec)| {
                rec.profile_name.as_ref().map(|p| TargetProcess {
                    pid: *pid,
                    profile: p.clone(),
                })
            })
            .collect()
    }

    /// Number of entries currently in the table — for telemetry.
    pub fn tracked(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    /// Drift correction: walk /proc once and reconcile against the
    /// in-memory table. Catches:
    ///   - phantoms — table entries whose PID no longer exists (we
    ///     missed an EXIT event because the kernel dropped it under
    ///     load, or it fell off the truncated SO_RCVBUF queue);
    ///   - newcomers — matching procs that never showed up in EXEC
    ///     events (same kind of drop; or they raced with the seed walk
    ///     at startup).
    ///
    /// Returns `(added, dropped)` for the operator's log line. Cheap
    /// when the table matches /proc — at worst one classify_pid per
    /// candidate, all done outside the lock.
    pub fn reseed_drift_correction(&self) -> (usize, usize) {
        let live_pids: HashSet<u32> = match fs::read_dir("/proc") {
            Ok(d) => d
                .flatten()
                .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
                .collect(),
            Err(e) => {
                warn!(err = %e, "drift correction: cannot read /proc");
                return (0, 0);
            }
        };

        let existing_pids: HashSet<u32> = self.inner.read().unwrap().keys().copied().collect();

        let phantoms: Vec<u32> = existing_pids.difference(&live_pids).copied().collect();
        let candidates: Vec<u32> = live_pids.difference(&existing_pids).copied().collect();

        // Classify outside the write lock so the cn_proc reader thread
        // is not blocked on /proc/PID/cmdline reads.
        let mut new_records: Vec<(u32, ProcessRecord)> = Vec::new();
        for pid in candidates {
            if let Some(r) = self.classify_pid(pid) {
                new_records.push((pid, r));
            }
        }

        let mut table = self.inner.write().unwrap();
        let mut dropped = 0usize;
        for pid in &phantoms {
            if table.remove(pid).is_some() {
                dropped += 1;
            }
        }
        let added = new_records.len();
        for (pid, r) in new_records {
            table.insert(pid, r);
        }
        (added, dropped)
    }

    /// Signal the reader thread to exit at its next loop iteration.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}
