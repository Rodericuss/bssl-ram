#![allow(dead_code, unused_imports, clippy::all)]

#[path = "../src/compressor.rs"]
mod compressor;
#[path = "../src/scanner.rs"]
mod scanner;

use std::fs;

fn parse_anon_regions(pid: u32) -> Vec<(usize, usize, String, String, String, u64)> {
    let smaps = fs::read_to_string(format!("/proc/{}/smaps", pid)).unwrap_or_default();
    let mut out = Vec::new();
    let mut header: Option<(usize, usize, String, String, String)> = None;
    let mut anon_kb: u64 = 0;

    for line in smaps.lines() {
        if let Some(dash) = line.find('-') {
            let rest = &line[dash + 1..];
            if let Some(sp) = rest.find(' ') {
                let end_str = &rest[..sp];
                let after = &rest[sp + 1..];
                let parts: Vec<&str> = after.split_whitespace().collect();
                if parts.len() >= 4 {
                    if let Some((s, e, perms, inode, name)) = header.take() {
                        if anon_kb > 0 {
                            out.push((s, e - s, perms, inode, name, anon_kb));
                        }
                    }
                    if let (Ok(start), Ok(end)) = (
                        usize::from_str_radix(&line[..dash], 16),
                        usize::from_str_radix(end_str, 16),
                    ) {
                        let perms = parts[0].to_string();
                        let inode = parts[3].to_string();
                        let name = parts.get(4).copied().unwrap_or("").to_string();
                        header = Some((start, end, perms, inode, name));
                        anon_kb = 0;
                    }
                }
            }
        }
        if line.starts_with("Anonymous:") {
            anon_kb = line
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
    }
    if let Some((s, e, perms, inode, name)) = header {
        if anon_kb > 0 {
            out.push((s, e - s, perms, inode, name, anon_kb));
        }
    }
    out
}

fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    let tabs = scanner::scan_targets(&scanner::default_profiles());
    let target = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<u32>().ok())
        .or_else(|| {
            tabs.iter()
                .max_by_key(|t| compressor::rss_mib(t.pid))
                .map(|t| t.pid)
        })
        .expect("no target pid");

    println!("=== compress dry-run for pid {} ===", target);
    let rss = compressor::rss_mib(target);
    println!("RSS: {} MiB", rss);

    let regions = parse_anon_regions(target);
    println!(
        "\nFound {} anonymous regions with anon pages:",
        regions.len()
    );

    let mut by_perms: std::collections::HashMap<String, (usize, u64)> = Default::default();
    let mut total_kb = 0u64;
    let mut shared = 0u64;
    let mut not_anon_inode = 0u64;

    for (_start, size, perms, inode, _name, anon_kb) in &regions {
        let entry = by_perms.entry(perms.clone()).or_default();
        entry.0 += 1;
        entry.1 += anon_kb;
        total_kb += anon_kb;
        if !perms.contains('p') {
            shared += anon_kb;
        }
        if inode != "0" {
            not_anon_inode += anon_kb;
        }
        let _ = size;
    }

    println!("Total anon kb: {} ({} MiB)", total_kb, total_kb / 1024);
    println!("Shared (NOT 'p') kb: {}", shared);
    println!(
        "File-backed (inode != 0) but with anon kb: {}",
        not_anon_inode
    );
    println!("\nBy permission set:");
    for (perms, (count, kb)) in &by_perms {
        println!("  {}  count={:>4}  anon_kb={:>10}", perms, count, kb);
    }

    println!("\nCalling compress_pid(dry_run=true)...");
    if let Err(e) = compressor::compress_pid(target, true) {
        eprintln!("error: {e}");
    }
}
