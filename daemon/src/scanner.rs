use std::fs;

/// A Firefox content process eligible for compression
#[derive(Debug, Clone)]
pub struct FirefoxTabProcess {
    pub pid: u32,
}

/// Scans /proc for Firefox tab content processes.
///
/// We identify them by two conditions in /proc/PID/cmdline:
///   1. Contains "-isForBrowser" — marks it as a browser content process
///   2. Last two tokens are "<number> tab" — confirms it's a tab process
///
/// This excludes: rdd, utility, socket, gpu, forkserver processes
/// which are Firefox infrastructure, not tab renderers.
pub fn scan_firefox_tabs() -> Vec<FirefoxTabProcess> {
    let mut result = Vec::new();

    let proc_dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return result,
    };

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only numeric directories (PIDs)
        let pid: u32 = match name_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        let cmdline_path = format!("/proc/{}/cmdline", pid);
        let cmdline = match fs::read(&cmdline_path) {
            Ok(b) => b,
            Err(_) => continue, // process may have exited
        };

        // cmdline is null-separated — split on \0
        let args: Vec<&str> = cmdline
            .split(|&b| b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .filter(|s| !s.is_empty())
            .collect();

        // Must be a Firefox process
        if !args.first().map_or(false, |a| a.contains("firefox")) {
            continue;
        }

        // Must have -isForBrowser flag
        if !args.iter().any(|a| *a == "-isForBrowser") {
            continue;
        }

        // Last token must be "tab"
        if args.last().map_or(true, |a| *a != "tab") {
            continue;
        }

        result.push(FirefoxTabProcess { pid });
    }

    result
}
