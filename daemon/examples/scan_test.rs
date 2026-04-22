#![allow(dead_code, unused_imports, clippy::all)]

#[path = "../src/scanner.rs"]
mod scanner;

fn main() {
    let tabs = scanner::scan_firefox_tabs();
    println!("scanner found {} tabs", tabs.len());
    let mut pids: Vec<u32> = tabs.iter().map(|t| t.pid).collect();
    pids.sort();
    for pid in pids {
        println!("{}", pid);
    }
}
