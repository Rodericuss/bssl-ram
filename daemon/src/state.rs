use std::collections::{HashMap, HashSet};

/// Snapshot of a process's CPU position for the idle / wakeup deltas.
///
/// Both fields use *nanoseconds since some monotonic origin* — the
/// tracker only ever cares about deltas, so any source works as long
/// as it is monotonic per task.
///
/// Where the numbers come from in v0.3.0+:
///
/// * `cpu_ns` — preferred from the eBPF cpu_tracker map (kernel
///   accumulates per-tgid runtime on every sched_switch). Falls back
///   to `(utime + stime) * (1e9 / USER_HZ)` parsed out of
///   `/proc/PID/stat` when BPF is unavailable.
/// * `starttime` — preferred from BPF (`task_struct->start_time`,
///   already nanoseconds-since-boot). Falls back to field 22 of
///   `/proc/PID/stat` (in jiffies, but we compare equality only so
///   the unit is irrelevant).
///
/// Tracking starttime per snapshot lets the daemon detect PID reuse:
/// if the kernel recycles a PID the new task's starttime is different,
/// and we treat that as a fresh observation rather than inheriting
/// stale CPU deltas / idle counters / compressed flags.
#[derive(Debug, Clone)]
pub struct ProcSnapshot {
    pub pid: u32,
    pub starttime: u64,
    pub cpu_ns: u64,
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
    /// Update snapshot and return whether the process is currently idle.
    ///
    /// Two thresholds, two questions, both in **nanoseconds**:
    ///
    ///   * `cpu_delta_threshold_ns` answers **"is this idle right
    ///     now?"** — a small budget (default 20 000 000 ns ≈ 20ms of
    ///     CPU per cycle). Below it, the idle-cycle counter accrues
    ///     toward compression; above it, the counter resets.
    ///
    ///   * `wakeup_delta_threshold_ns` answers **"did the user actually
    ///     interact with this process?"** — a much higher bar (default
    ///     500 000 000 ns ≈ 500ms of CPU per cycle). Only crossing this
    ///     clears the anti-recompression flag.
    ///
    /// Why two? Firefox / Chromium content procs fire GC, service-worker
    /// pulses, and internal timers that briefly burn 5–30ms even while
    /// the tab is unattended. Without a separate wakeup bar, every such
    /// micro-burst clears the `compressed` flag and the next idle cycle
    /// re-issues `process_madvise` on pages that are already in zram —
    /// burning CPU and growing zstd churn for no benefit. Live repro
    /// showed PIDs being compressed 3× in 90s; with the dual threshold
    /// each PID is compressed once until real user activity.
    pub fn update(
        &mut self,
        snap: ProcSnapshot,
        cpu_delta_threshold_ns: u64,
        wakeup_delta_threshold_ns: u64,
    ) -> bool {
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

        let delta = if let Some(prev) = self.snapshots.get(&pid) {
            snap.cpu_ns.saturating_sub(prev.cpu_ns)
        } else {
            // First snapshot — no delta available. Treat as active so a
            // freshly-spawned tab isn't compressed before we have any
            // idea what it's doing.
            u64::MAX
        };
        let idle = delta <= cpu_delta_threshold_ns;
        let woke_up = delta > wakeup_delta_threshold_ns && delta != u64::MAX;

        self.snapshots.insert(pid, snap);

        if idle {
            *self.idle_cycles.entry(pid).or_insert(0) += 1;
        } else {
            // Active this cycle (delta above the *idle* bar) — restart
            // the warmup counter so we don't compress on stale idleness.
            self.idle_cycles.insert(pid, 0);
        }

        if woke_up {
            // Real user activity (delta above the *wakeup* bar) — the
            // process probably faulted in pages we paged out, so it
            // makes sense to consider it for compression again the next
            // time it goes idle. Pulses below this bar (Firefox GC,
            // service worker tick, browser timers, …) leave the flag
            // alone, killing the recompression cascade.
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

    /// Tests pre-v0.3.0 expressed CPU as `(utime_ticks, stime_ticks)`.
    /// Multiplying the sum by 10 000 000 keeps the same numeric meaning
    /// in the new ns world (1 tick = 10ms = 10 000 000 ns at the
    /// default USER_HZ=100), so the threshold values in the assertions
    /// below scale linearly without changing semantics.
    const TICK_NS: u64 = 10_000_000;

    fn snap(pid: u32, u: u64, s: u64) -> ProcSnapshot {
        snap_at(pid, u, s, 1000)
    }

    fn snap_at(pid: u32, u: u64, s: u64, starttime: u64) -> ProcSnapshot {
        ProcSnapshot {
            pid,
            starttime,
            cpu_ns: (u + s) * TICK_NS,
        }
    }

    /// Convert a tick-based threshold into the ns the new API expects.
    fn ticks(t: u64) -> u64 {
        t * TICK_NS
    }

    #[test]
    fn first_update_assumes_active() {
        // No prior snapshot ⇒ no delta to compute ⇒ play it safe and
        // treat as active. Keeps a freshly-spawned tab from being instantly
        // compressed before we have any idea what it's doing.
        let mut t = CpuTracker::new();
        assert!(!t.update(snap(1, 100, 50), ticks(2), ticks(50)));
        assert_eq!(t.idle_cycles(1), 0);
    }

    #[test]
    fn idle_cycles_accumulate_then_reset_on_activity() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 100, 50), ticks(2), ticks(50)); // first sample
        assert!(t.update(snap(1, 100, 50), ticks(2), ticks(50))); // delta 0 — idle
        assert_eq!(t.idle_cycles(1), 1);
        assert!(t.update(snap(1, 101, 50), ticks(2), ticks(50))); // delta 1 — still under threshold
        assert_eq!(t.idle_cycles(1), 2);
        assert!(!t.update(snap(1, 200, 50), ticks(2), ticks(50))); // delta 99 — active, reset
        assert_eq!(t.idle_cycles(1), 0);
    }

    #[test]
    fn delta_exactly_at_threshold_is_idle() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        // total delta = 2 (utime+stime), threshold = 2 ⇒ "delta <= threshold" ⇒ idle
        assert!(t.update(snap(1, 1, 1), ticks(2), ticks(50)));
    }

    #[test]
    fn compressed_flag_persists_across_idle_cycles() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        t.mark_compressed(1);
        assert!(t.is_compressed(1));
        // Another idle tick must NOT clear the flag — the whole point is
        // to keep skipping this PID until it shows activity.
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        assert!(t.is_compressed(1));
    }

    #[test]
    fn compressed_flag_clears_on_activity() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        t.mark_compressed(1);
        assert!(t.is_compressed(1));
        t.update(snap(1, 100, 100), ticks(2), ticks(50)); // big delta ⇒ active
        assert!(!t.is_compressed(1));
    }

    #[test]
    fn small_burst_resets_idle_but_keeps_compressed_flag() {
        // The bug Rodrigo flagged + we reproduced live: Firefox content
        // procs fire small CPU bursts (GC, service worker pulses) of
        // 5–30 ticks even on tabs the user is not looking at. Pre-fix,
        // any delta above the *idle* threshold cleared the compressed
        // flag and the next idle cycle re-issued process_madvise on
        // pages that were already in zram. Post-fix, only deltas above
        // the *wakeup* threshold count as real activity.
        let mut t = CpuTracker::new();
        // Establish baseline + idle + compressed
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        t.mark_compressed(1);
        assert!(t.is_compressed(1));

        // Burst of 10 ticks (between idle=2 and wakeup=50). Tab is
        // technically "active" this cycle so idle_cycles drops to 0,
        // but the compressed flag MUST NOT be cleared — pages are
        // still in zram, the burst was browser internal noise.
        let still_idle = t.update(snap(1, 6, 4), ticks(2), ticks(50));
        assert!(
            !still_idle,
            "burst > cpu_delta_threshold ⇒ not idle this cycle"
        );
        assert_eq!(t.idle_cycles(1), 0, "idle warmup must restart");
        assert!(
            t.is_compressed(1),
            "small burst must NOT clear compressed flag — that was the recompression bug",
        );
    }

    #[test]
    fn real_wakeup_clears_compressed_flag() {
        // Counter-test: a delta well above the wakeup threshold (real
        // user interaction or a page faulting in lots of paged-out
        // memory) must clear compressed so the next idle period is
        // eligible for compression again.
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        t.mark_compressed(1);
        // Delta = 200 ticks (2s of CPU) — clearly real activity
        let still_idle = t.update(snap(1, 100, 100), ticks(2), ticks(50));
        assert!(!still_idle);
        assert!(
            !t.is_compressed(1),
            "delta > wakeup_threshold must clear flag"
        );
    }

    #[test]
    fn first_observation_does_not_count_as_wakeup() {
        // First snapshot has no prior delta to compare against. The old
        // code special-cased this as "active" (idle=false) and would
        // have cleared compressed. Post-fix, with no delta available we
        // model the gap as u64::MAX and skip the wakeup branch — there
        // can't be a meaningful "wakeup" without a prior measurement.
        let mut t = CpuTracker::new();
        // Pretend this PID was already marked compressed (e.g. carried
        // across a refactor / future state restore)
        t.mark_compressed(1);
        let idle = t.update(snap(1, 100, 100), ticks(2), ticks(50));
        assert!(!idle, "first observation is treated as active");
        assert!(
            t.is_compressed(1),
            "first observation must NOT clear an existing compressed flag",
        );
    }

    #[test]
    fn pid_reuse_wipes_inherited_state() {
        // Council-flagged #4: PID rollover means a brand-new task can
        // end up with the same PID as a compressed-and-forgotten one.
        // Without the starttime guard it would inherit the `compressed`
        // flag and never be considered for compression.
        let mut t = CpuTracker::new();

        // Old task: compressed, fully tracked
        t.update(snap_at(42, 100, 50, 1000), ticks(2), ticks(50));
        t.update(snap_at(42, 100, 50, 1000), ticks(2), ticks(50)); // idle
        t.mark_compressed(42);
        assert!(t.is_compressed(42));
        assert_eq!(t.idle_cycles(42), 1);

        // Same PID, but starttime advanced — kernel recycled it
        let still_idle = t.update(snap_at(42, 999, 999, 5000), ticks(2), ticks(50));

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
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        t.update(snap(2, 0, 0), ticks(2), ticks(50));
        t.update(snap(3, 0, 0), ticks(2), ticks(50));
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
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
        t.mark_compressed(1);
        t.retain_only(&HashSet::new());
        assert_eq!(t.tracked_pids(), 0);
    }

    #[test]
    fn remove_clears_all_tracking_state_for_pid() {
        let mut t = CpuTracker::new();
        t.update(snap(1, 0, 0), ticks(2), ticks(50));
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
        t.update(snap(1, 1000, 1000), ticks(2), ticks(50));
        assert!(t.update(snap(1, 100, 100), ticks(2), ticks(50))); // would underflow without saturating_sub
        assert_eq!(t.idle_cycles(1), 1);
    }
}
