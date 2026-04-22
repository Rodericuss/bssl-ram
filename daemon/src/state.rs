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
}

impl CpuTracker {
    pub fn new() -> Self {
        Self {
            snapshots: HashMap::new(),
            idle_cycles: HashMap::new(),
        }
    }

    /// Update snapshot for a PID and return whether it's idle.
    /// A process is idle if its CPU delta (utime + stime) is below
    /// the threshold across consecutive cycles.
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
        }

        idle
    }

    /// Returns how many consecutive idle cycles a PID has accumulated
    pub fn idle_cycles(&self, pid: u32) -> u32 {
        *self.idle_cycles.get(&pid).unwrap_or(&0)
    }

    /// Remove tracking data for a PID (process exited)
    pub fn remove(&mut self, pid: u32) {
        self.snapshots.remove(&pid);
        self.idle_cycles.remove(&pid);
    }
}
