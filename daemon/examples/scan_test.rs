#![allow(dead_code, unused_imports, clippy::all)]

#[path = "../src/scanner.rs"]
mod scanner;

fn main() {
    let targets = scanner::scan_targets(&scanner::default_profiles());
    println!("scanner found {} targets", targets.len());
    let mut rows: Vec<(u32, String)> = targets.iter().map(|t| (t.pid, t.profile.clone())).collect();
    rows.sort();
    for (pid, profile) in rows {
        println!("{:>7} [{}]", pid, profile);
    }
}
