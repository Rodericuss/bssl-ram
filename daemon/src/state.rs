use std::collections::HashMap;

/// Snapshot of /proc/PID/stat for CPU delta calculation
#[derive(Debug, Clone)]
pub struct ProcSnapshot {
    pub pid: u32,
    pub utime: u64,  // user mode ticks
    pub stime: u64,  // kernel mode ticks
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
        let idle = if let Some(prev) = self.snapshots.get(&pid) {
            let delta = (snap.utime + snap.stime)
                .saturating_sub(prev.utime + prev.stime);
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
}
