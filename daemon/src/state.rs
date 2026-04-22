use serde::Deserialize;
use std::collections::HashMap;

/// Message received from the Firefox extension via WebSocket
#[derive(Debug, Deserialize)]
pub struct TabReport {
    pub tabs: Vec<TabInfo>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TabInfo {
    pub pid: u32,
    pub idle_seconds: u64,
    pub url: String,
    pub active: bool,
}

/// Returns the set of PIDs eligible for compression.
///
/// A PID is eligible only if ALL tabs sharing that PID are idle
/// (handles Firefox Fission: multiple tabs can share one content process).
pub fn eligible_pids(report: &TabReport, threshold_secs: u64) -> Vec<u32> {
    // group tabs by pid
    let mut by_pid: HashMap<u32, Vec<&TabInfo>> = HashMap::new();
    for tab in &report.tabs {
        by_pid.entry(tab.pid).or_default().push(tab);
    }

    by_pid
        .into_iter()
        .filter(|(_, tabs)| {
            // every tab in this process must be idle above threshold
            tabs.iter().all(|t| !t.active && t.idle_seconds >= threshold_secs)
        })
        .map(|(pid, _)| pid)
        .collect()
}
