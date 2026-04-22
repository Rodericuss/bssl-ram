use std::collections::{HashMap, HashSet};

/// Snapshot of /proc/PID/stat for CPU delta calculation.
///
/// `starttime` (field 22 of /proc/PID/stat, in jiffies since boot) is
/// tracked per snapshot so the tracker can detect PID reuse: if the
/// kernel recycles a PID, the new task's starttime is strictly greater
/// than the old one, and we treat that as a fresh observation instead
/// of inheriting stale CPU deltas / idle counters / compressed flags.
#[derive(Debug, Clone)]
pub struct ProcSnapshot {
    pub pid: u32,
    pub starttime: u64,
    pub utime: u64, // user mode ticks
    pub stime: u64, // kernel mode ticks
}

/// Tracks CPU usage history per PID to detect idle processes
pub struct CpuTracker {
    /// pid → last snapshot
    snapshots: HashMap<u32, ProcSnapshot>,
    /// pid → how many consecutive idle cycles
    idle_cycles: HashMap<u32, u32>,
    /// pid → already compressed since last active period
    /// Cleared the moment this PID shows a non-idle delta, so the daemon
    /// will compress it again the *next* time it goes idle — but not
    /// repeatedly while it stays idle.
    compressed: HashMap<u32, bool>,
}

impl CpuTracker {
    pub fn new() -> Self {
        Self {
            snapshots: HashMap::new(),
            idle_cycles: HashMap::new(),
            compressed: HashMap::new(),
        }
    }

    /// Update snapshot for a PID and return whether it's idle.
    /// A process is idle if its CPU delta (utime + stime) is below
    /// the threshold across consecutive cycles.
    ///
    /// Side effect: when a non-idle delta is seen, the "already compressed"
    /// flag is cleared — the process has woken up, so a future idle period
    /// is eligible for compression again.
    pub fn update(&mut self, snap: ProcSnapshot, cpu_delta_threshold: u64) -> bool {
        let pid = snap.pid;

        // PID-reuse guard. If we already know about this PID but the
        // starttime doesn't match, the kernel recycled the PID for a
        // different task — every byte of state we kept around belongs
        // to a process that no longer exists. Wiping it here means the
        // fresh task will be treated as a first-time observation and
        // won't inherit an "already compressed" flag it never earned.
        if let Some(prev) = self.snapshots.get(&pid) {
            if prev.starttime != snap.starttime {
                self.snapshots.remove(&pid);
                self.idle_cycles.remove(&pid);
                self.compressed.remove(&pid);
            }
        }

        let idle = if let Some(prev) = self.snapshots.get(&pid) {
            let delta = (snap.utime + snap.stime).saturating_sub(prev.utime + prev.stime);
            delta <= cpu_delta_threshold
        } else {
            // First snapshot — not enough data, assume active
            false
        };

        self.snapshots.insert(pid, snap);

        if idle {
            *self.idle_cycles.entry(pid).or_insert(0) += 1;
        } else {
            self.idle_cycles.insert(pid, 0);
            // Woke up — eligible for compression again next time it idles
            self.compressed.remove(&pid);
        }

        idle
    }

    /// Returns how many consecutive idle cycles a PID has accumulated
    pub fn idle_cycles(&self, pid: u32) -> u32 {
        *self.idle_cycles.get(&pid).unwrap_or(&0)
    }

    /// Returns true if this PID has already been compressed since it last
    /// showed any CPU activity. Callers should skip compression in that
    /// case — the pages are already in zram, calling MADV_PAGEOUT again
    /// only wastes syscalls and walks the same (mostly empty) page tables.
    pub fn is_compressed(&self, pid: u32) -> bool {
        self.compressed.get(&pid).copied().unwrap_or(false)
    }

    /// Mark this PID as compressed. Call this after a successful
    /// compress_pid() so the next scan cycle skips it.
    pub fn mark_compressed(&mut self, pid: u32) {
        self.compressed.insert(pid, true);
    }

    /// Remove tracking data for a PID (process exited)
    pub fn remove(&mut self, pid: u32) {
        self.snapshots.remove(&pid);
        self.idle_cycles.remove(&pid);
        self.compressed.remove(&pid);
    }

    /// Drop tracking state for every PID not in `live_pids`.
    ///
    /// Called at the end of each scan_cycle to bound memory and to
    /// forget short-lived PIDs that exited between cycles. The
    /// starttime check in `update()` already catches PID reuse for
    /// PIDs we see again; this sweeps the orphans that never come back.
    pub fn retain_only(&mut self, live_pids: &HashSet<u32>) {
        self.snapshots.retain(|pid, _| live_pids.contains(pid));
        self.idle_cycles.retain(|pid, _| live_pids.contains(pid));
        self.compressed.retain(|pid, _| live_pids.contains(pid));
    }

    /// Current number of PIDs held in the tracker. Mostly useful for
    /// telemetry and tests that assert the GC actually runs.
    pub fn tracked_pids(&self) -> usize {
        self.snapshots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(pid: u32, u: u64, s: u64) -> ProcSnapshot {
        snap_at(pid, u, s, 1000)
    }

    fn snap_at(pid: u32, u: u64, s: u64, starttime: u64) -> ProcSnapshot {
        ProcSnapshot {
            pid,
            starttime,
            utime: u,
            stime: s,
        }
    }

    #[test]
    fn first_update_assumes_active() {
        // No prior snapshot ⇒ no delta to compute ⇒ play it safe and
        // treat as active. Keeps a freshly-spawned tab from being instantly
        // compressed before we have any idea what it's doing.
        let mut t = CpuTracker::new();
        assert!(!t.update(snap(1, 100, 50), 2));
        assert_eq!(t.idle_cycles(1), 0);
    }

    #[test]
    fn idle_cycles_accumulate_then_reset_on_activity() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 100, 50), 2); // first sample
        assert!(t.update(snap(1, 100, 50), 2)); // delta 0 — idle
        assert_eq!(t.idle_cycles(1), 1);
        assert!(t.update(snap(1, 101, 50), 2)); // delta 1 — still under threshold
        assert_eq!(t.idle_cycles(1), 2);
        assert!(!t.update(snap(1, 200, 50), 2)); // delta 99 — active, reset
        assert_eq!(t.idle_cycles(1), 0);
    }

    #[test]
    fn delta_exactly_at_threshold_is_idle() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), 2);
        // total delta = 2 (utime+stime), threshold = 2 ⇒ "delta <= threshold" ⇒ idle
        assert!(t.update(snap(1, 1, 1), 2));
    }

    #[test]
    fn compressed_flag_persists_across_idle_cycles() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), 2);
        t.update(snap(1, 0, 0), 2);
        t.mark_compressed(1);
        assert!(t.is_compressed(1));
        // Another idle tick must NOT clear the flag — the whole point is
        // to keep skipping this PID until it shows activity.
        t.update(snap(1, 0, 0), 2);
        assert!(t.is_compressed(1));
    }

    #[test]
    fn compressed_flag_clears_on_activity() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), 2);
        t.mark_compressed(1);
        assert!(t.is_compressed(1));
        t.update(snap(1, 100, 100), 2); // big delta ⇒ active
        assert!(!t.is_compressed(1));
    }

    #[test]
    fn pid_reuse_wipes_inherited_state() {
        // Council-flagged #4: PID rollover means a brand-new task can
        // end up with the same PID as a compressed-and-forgotten one.
        // Without the starttime guard it would inherit the `compressed`
        // flag and never be considered for compression.
        let mut t = CpuTracker::new();

        // Old task: compressed, fully tracked
        t.update(snap_at(42, 100, 50, 1000), 2);
        t.update(snap_at(42, 100, 50, 1000), 2); // idle
        t.mark_compressed(42);
        assert!(t.is_compressed(42));
        assert_eq!(t.idle_cycles(42), 1);

        // Same PID, but starttime advanced — kernel recycled it
        let still_idle = t.update(snap_at(42, 999, 999, 5000), 2);

        // Fresh task: no inherited state
        assert!(!still_idle, "new task must be treated as first observation");
        assert!(
            !t.is_compressed(42),
            "compressed flag must NOT carry over on PID reuse"
        );
        assert_eq!(t.idle_cycles(42), 0);
    }

    #[test]
    fn retain_only_drops_pids_not_in_live_set() {
        use std::collections::HashSet;
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), 2);
        t.update(snap(2, 0, 0), 2);
        t.update(snap(3, 0, 0), 2);
        t.mark_compressed(1);
        t.mark_compressed(2);

        let live: HashSet<u32> = [2u32, 3].into_iter().collect();
        t.retain_only(&live);

        assert_eq!(t.tracked_pids(), 2);
        assert!(!t.is_compressed(1), "pid 1 state must be gone after GC");
        assert!(t.is_compressed(2), "pid 2 state must survive GC");
        assert_eq!(t.idle_cycles(1), 0, "pid 1 idle counter must be gone");
    }

    #[test]
    fn retain_only_with_empty_live_set_clears_everything() {
        use std::collections::HashSet;
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), 2);
        t.mark_compressed(1);
        t.retain_only(&HashSet::new());
        assert_eq!(t.tracked_pids(), 0);
    }

    #[test]
    fn remove_clears_all_tracking_state_for_pid() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), 2);
        t.mark_compressed(1);
        t.remove(1);
        assert!(!t.is_compressed(1));
        assert_eq!(t.idle_cycles(1), 0);
    }

    #[test]
    fn saturating_sub_protects_against_counter_wrap() {
        // Can't realistically happen on Linux (utime is monotonic per
        // process), but the saturating_sub means a buggy reader giving us
        // smaller "current" values doesn't underflow into a huge delta
        // that would falsely flag the process as wildly active.
        let mut t = CpuTracker::new();
        t.update(snap(1, 1000, 1000), 2);
        assert!(t.update(snap(1, 100, 100), 2)); // would underflow without saturating_sub
        assert_eq!(t.idle_cycles(1), 1);
    }
}
