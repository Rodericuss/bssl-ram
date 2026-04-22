use std::fs;

/// A Firefox content process eligible for compression
#[derive(Debug, Clone)]
pub struct FirefoxTabProcess {
    pub pid: u32,
}

/// Tokenises a raw /proc/PID/cmdline blob.
///
/// The kernel separates argv elements with NUL by default, but Firefox
/// tab content processes rewrite their argv into a single space-separated
/// string so they look nice in `ps`. Treating NUL as whitespace lets us
/// tokenise both forms with the same rule.
pub fn parse_cmdline(raw: &[u8]) -> Vec<String> {
    raw.iter()
        .map(|&b| if b == 0 { ' ' } else { b as char })
        .collect::<String>()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

/// Returns true when the cmdline tokens describe a Firefox **tab** content
/// process (not rdd, utility, socket, gpu, or forkserver).
pub fn is_firefox_tab(args: &[String]) -> bool {
    args.first().is_some_and(|a| a.contains("firefox"))
        && args.iter().any(|a| a == "-isForBrowser")
        && args.last().is_some_and(|a| a == "tab")
}

/// Scans /proc for Firefox tab content processes by walking every numeric
/// entry, parsing the cmdline, and applying [`is_firefox_tab`].
pub fn scan_firefox_tabs() -> Vec<FirefoxTabProcess> {
    let mut result = Vec::new();

    let proc_dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return result,
    };

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        let pid: u32 = match name_str.parse() {
            Ok(n) => n,
            Err(_) => continue,
        };

        let cmdline_raw = match fs::read(format!("/proc/{}/cmdline", pid)) {
            Ok(b) => b,
            Err(_) => continue, // process may have exited
        };

        let args = parse_cmdline(&cmdline_raw);
        if is_firefox_tab(&args) {
            result.push(FirefoxTabProcess { pid });
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parse_cmdline_handles_nul_separated_form() {
        // Real layout used by infrastructure procs (rdd, utility, etc.)
        let raw = b"/usr/lib/firefox/firefox\0-contentproc\0-isForBrowser\0tab\0";
        assert_eq!(
            parse_cmdline(raw),
            s(&[
                "/usr/lib/firefox/firefox",
                "-contentproc",
                "-isForBrowser",
                "tab"
            ])
        );
    }

    #[test]
    fn parse_cmdline_handles_space_separated_firefox_tab_form() {
        // Real layout used by Firefox tab processes (argv rewritten for ps)
        let raw = b"/usr/lib/firefox/firefox -contentproc -isForBrowser 3 tab";
        assert_eq!(
            parse_cmdline(raw),
            s(&[
                "/usr/lib/firefox/firefox",
                "-contentproc",
                "-isForBrowser",
                "3",
                "tab"
            ])
        );
    }

    #[test]
    fn parse_cmdline_handles_mixed_and_trailing_nul() {
        // Some procs have a trailing NUL plus internal NULs — must not
        // produce empty tokens.
        let raw = b"firefox\0-isForBrowser\0\0tab\0";
        assert_eq!(parse_cmdline(raw), s(&["firefox", "-isForBrowser", "tab"]));
    }

    #[test]
    fn is_firefox_tab_accepts_real_tab_process() {
        let args = s(&[
            "/usr/lib/firefox/firefox",
            "-contentproc",
            "-isForBrowser",
            "tab",
        ]);
        assert!(is_firefox_tab(&args));
    }

    #[test]
    fn is_firefox_tab_rejects_rdd_process() {
        // rdd has -isForBrowser but ends in "rdd", not "tab"
        let args = s(&[
            "/usr/lib/firefox/firefox",
            "-contentproc",
            "-isForBrowser",
            "rdd",
        ]);
        assert!(!is_firefox_tab(&args));
    }

    #[test]
    fn is_firefox_tab_rejects_non_firefox_process_ending_in_tab() {
        // Some other process whose argv ends in "tab" — must not match
        let args = s(&["/usr/bin/editor", "-isForBrowser", "tab"]);
        assert!(!is_firefox_tab(&args));
    }

    #[test]
    fn is_firefox_tab_rejects_firefox_main_process() {
        // The main Firefox UI has no -isForBrowser flag
        let args = s(&["/usr/lib/firefox/firefox"]);
        assert!(!is_firefox_tab(&args));
    }

    #[test]
    fn is_firefox_tab_rejects_empty_cmdline() {
        assert!(!is_firefox_tab(&[]));
    }
}
