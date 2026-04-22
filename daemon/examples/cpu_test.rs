#[path = "../src/scanner.rs"]
mod scanner;
#[path = "../src/compressor.rs"]
mod compressor;

use std::collections::HashMap;
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let tabs = scanner::scan_firefox_tabs();
    println!("tracking {} tabs", tabs.len());

    let mut prev: HashMap<u32, (u64, u64)> = HashMap::new();
    for t in &tabs {
        if let Some(c) = compressor::read_cpu_ticks(t.pid) {
            prev.insert(t.pid, c);
        }
    }

    let cycles = std::env::var("CYCLES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5u32);
    let interval_secs = std::env::var("INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2u64);

    for cycle in 1..=cycles {
        sleep(Duration::from_secs(interval_secs));
        println!("\n=== cycle {} (interval {}s) ===", cycle, interval_secs);
        for t in &tabs {
            let pid = t.pid;
            let cur = match compressor::read_cpu_ticks(pid) {
                Some(c) => c,
                None => {
                    println!("pid {:>7} EXITED", pid);
                    continue;
                }
            };
            let p = prev.get(&pid).copied().unwrap_or((cur.0, cur.1));
            let delta = (cur.0 + cur.1).saturating_sub(p.0 + p.1);
            let rss = compressor::rss_mib(pid);
            println!(
                "pid {:>7} u={:>6} s={:>6} Δticks={:>4} ({}ms cpu) rss={}MiB",
                pid,
                cur.0,
                cur.1,
                delta,
                delta * 10,
                rss
            );
            prev.insert(pid, cur);
        }
    }
}
