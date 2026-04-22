#[path = "../src/scanner.rs"]
mod scanner;
#[path = "../src/compressor.rs"]
mod compressor;

use std::fs;
use std::thread::sleep;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
struct RollupStats {
    rss_kb: u64,
    pss_kb: u64,
    swap_kb: u64,
    anonymous_kb: u64,
}

fn smaps_rollup(pid: u32) -> RollupStats {
    let mut s = RollupStats::default();
    let raw = match fs::read_to_string(format!("/proc/{}/smaps_rollup", pid)) {
        Ok(s) => s,
        Err(_) => return s,
    };
    for line in raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let val: u64 = parts[1].parse().unwrap_or(0);
        match parts[0] {
            "Rss:" => s.rss_kb = val,
            "Pss:" => s.pss_kb = val,
            "Swap:" => s.swap_kb = val,
            "Anonymous:" => s.anonymous_kb = val,
            _ => {}
        }
    }
    s
}

fn print_stats(label: &str, s: &RollupStats) {
    println!(
        "{:<10} RSS={:>7} KiB ({:>5} MiB) | PSS={:>7} | Swap={:>7} | Anon={:>7}",
        label,
        s.rss_kb,
        s.rss_kb / 1024,
        s.pss_kb,
        s.swap_kb,
        s.anonymous_kb
    );
}

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    let tabs = scanner::scan_firefox_tabs();
    println!("found {} tab procs", tabs.len());

    // Pick the largest RSS tab as target — most signal to noise
    let target_pid = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<u32>().ok())
        .or_else(|| {
            tabs.iter()
                .max_by_key(|t| compressor::rss_mib(t.pid))
                .map(|t| t.pid)
        })
        .expect("no firefox tabs found");

    println!("\n=== target pid {} ===", target_pid);
    let before = smaps_rollup(target_pid);
    print_stats("BEFORE", &before);

    println!("\nrunning compress_pid(dry_run=false)...");
    let started = Instant::now();
    if let Err(e) = compressor::compress_pid(target_pid, false) {
        eprintln!("compress_pid error: {e}");
        std::process::exit(1);
    }
    println!("syscalls completed in {:?}", started.elapsed());

    // Wait a moment for accounting to update
    sleep(Duration::from_millis(500));
    let after = smaps_rollup(target_pid);
    print_stats("AFTER", &after);

    let rss_drop = before.rss_kb.saturating_sub(after.rss_kb);
    let swap_gain = after.swap_kb.saturating_sub(before.swap_kb);
    let pss_drop = before.pss_kb.saturating_sub(after.pss_kb);

    println!("\n=== delta ===");
    println!(
        "RSS:  -{} KiB ({} MiB)   PSS: -{} KiB   Swap: +{} KiB ({} MiB)",
        rss_drop,
        rss_drop / 1024,
        pss_drop,
        swap_gain,
        swap_gain / 1024
    );

    if rss_drop == 0 && swap_gain == 0 {
        println!("\nWARNING: nothing changed — check perms/syscall");
    } else {
        let ratio = if before.rss_kb > 0 {
            (rss_drop * 100) / before.rss_kb
        } else {
            0
        };
        println!("→ paged out {}% of pre-compression RSS", ratio);
    }

    // Show /proc/swaps current state
    println!("\n--- /proc/swaps after ---");
    if let Ok(s) = fs::read_to_string("/proc/swaps") {
        print!("{}", s);
    }
}
