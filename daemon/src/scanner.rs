use serde::Deserialize;
use std::fs;

/// One target process matched by a [`BrowserProfile`].
#[derive(Debug, Clone)]
pub struct TargetProcess {
    pub pid: u32,
    pub profile: String,
}

/// Declarative match rule for a browser/app family.
///
/// Profiles are config-driven so we can support new browsers and Electron
/// apps without recompiling. The match logic is pure (see [`match_profile`])
/// and unit-tested with real cmdline fixtures.
#[derive(Debug, Deserialize, Clone)]
pub struct BrowserProfile {
    /// Human-readable name, used in logs and metrics.
    pub name: String,

    /// `argv[0]` (the executable path) must contain ANY of these
    /// case-insensitive substrings. Empty list = match any binary
    /// (rely on flags alone, useful for "any Chromium-based renderer").
    #[serde(default)]
    pub binary_substring_any: Vec<String>,

    /// ALL of these tokens must appear somewhere in argv. Exact equality.
    #[serde(default)]
    pub arg_required_all: Vec<String>,

    /// If ANY of these tokens appears, the process is excluded. Used to
    /// filter out extension renderers, service worker renderers, etc.
    #[serde(default)]
    pub arg_excluded_any: Vec<String>,

    /// Optional: the LAST argv element must equal this value exactly.
    /// Firefox tab content procs end in `tab` — `rdd`, `socket`, `gpu`,
    /// `forkserver`, etc. all have `-isForBrowser` too but a different
    /// trailing token, so this is the cleanest discriminator.
    #[serde(default)]
    pub arg_last: Option<String>,
}

/// Default profiles shipped with the daemon. Covers the major desktop
/// browsers + Electron apps. Users can override or extend via
/// `/etc/bssl-ram/config.toml [[profiles]]` blocks.
pub fn default_profiles() -> Vec<BrowserProfile> {
    vec![
        // Firefox-family. The `-isForBrowser` flag + trailing `tab`
        // discriminates content tabs from rdd/utility/gpu/socket/forkserver.
        BrowserProfile {
            name: "firefox".into(),
            binary_substring_any: vec![
                "firefox".into(),
                "librewolf".into(),
                "waterfox".into(),
                "icecat".into(),
                "zen".into(),
            ],
            arg_required_all: vec!["-isForBrowser".into()],
            arg_excluded_any: vec![],
            arg_last: Some("tab".into()),
        },
        // Chromium-family + Electron apps in one shot. All Chromium-based
        // renderers carry `--type=renderer`; extension renderers
        // additionally carry `--extension-process` and are excluded
        // (extension lifecycle != tab content lifecycle, paging them out
        // can cause noticeable stalls when popups open).
        //
        // No binary filter on purpose: this matches Chrome, Chromium,
        // Brave, Edge, Vivaldi, Opera, Yandex, Thorium, AND every Electron
        // app (VS Code, Discord, Slack, Spotify, Obsidian, Signal, Notion,
        // Element, Teams, Vesktop, ...). The argv pattern itself is
        // distinctive enough — anything else launching with
        // `--type=renderer` is a Chromium derivative and a fair target.
        BrowserProfile {
            name: "chromium".into(),
            binary_substring_any: vec![],
            arg_required_all: vec!["--type=renderer".into()],
            arg_excluded_any: vec!["--extension-process".into()],
            arg_last: None,
        },
    ]
}

/// Tokenises a raw `/proc/PID/cmdline` blob.
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

/// Pure profile-matcher. Returns true when every constraint of the profile
/// is satisfied by `args`. Order of checks is from cheapest to most
/// expensive so the common rejection path returns fast.
pub fn match_profile(args: &[String], profile: &BrowserProfile) -> bool {
    if args.is_empty() {
        return false;
    }

    if !profile.binary_substring_any.is_empty() {
        let bin = args[0].to_ascii_lowercase();
        if !profile
            .binary_substring_any
            .iter()
            .any(|s| bin.contains(&s.to_ascii_lowercase()))
        {
            return false;
        }
    }

    for required in &profile.arg_required_all {
        if !args.iter().any(|a| a == required) {
            return false;
        }
    }

    for excluded in &profile.arg_excluded_any {
        if args.iter().any(|a| a == excluded) {
            return false;
        }
    }

    if let Some(last) = &profile.arg_last {
        if args.last() != Some(last) {
            return false;
        }
    }

    true
}

/// Walks `/proc`, parses each cmdline, and returns every process matched
/// by ANY of the supplied profiles. The first matching profile wins
/// (profiles are evaluated in order, so put the most specific ones first).
pub fn scan_targets(profiles: &[BrowserProfile]) -> Vec<TargetProcess> {
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
        if let Some(p) = profiles.iter().find(|p| match_profile(&args, p)) {
            result.push(TargetProcess {
                pid,
                profile: p.name.clone(),
            });
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

    fn firefox() -> BrowserProfile {
        default_profiles()
            .into_iter()
            .find(|p| p.name == "firefox")
            .unwrap()
    }

    fn chromium() -> BrowserProfile {
        default_profiles()
            .into_iter()
            .find(|p| p.name == "chromium")
            .unwrap()
    }

    // --- parse_cmdline ---------------------------------------------------

    #[test]
    fn parse_cmdline_handles_nul_separated_form() {
        let raw = b"/usr/lib/firefox/firefox\0-contentproc\0-isForBrowser\0tab\0";
        assert_eq!(
            parse_cmdline(raw),
            s(&[
                "/usr/lib/firefox/firefox",
                "-contentproc",
                "-isForBrowser",
                "tab",
            ])
        );
    }

    #[test]
    fn parse_cmdline_handles_space_separated_firefox_tab_form() {
        // Firefox rewrites argv with literal spaces for nicer `ps` output
        let raw = b"/usr/lib/firefox/firefox -contentproc -isForBrowser 3 tab";
        assert_eq!(
            parse_cmdline(raw),
            s(&[
                "/usr/lib/firefox/firefox",
                "-contentproc",
                "-isForBrowser",
                "3",
                "tab",
            ])
        );
    }

    #[test]
    fn parse_cmdline_handles_mixed_and_trailing_nul() {
        let raw = b"firefox\0-isForBrowser\0\0tab\0";
        assert_eq!(parse_cmdline(raw), s(&["firefox", "-isForBrowser", "tab"]));
    }

    // --- firefox profile -------------------------------------------------

    #[test]
    fn firefox_profile_matches_real_tab_process() {
        let args = s(&[
            "/usr/lib/firefox/firefox",
            "-contentproc",
            "-isForBrowser",
            "tab",
        ]);
        assert!(match_profile(&args, &firefox()));
    }

    #[test]
    fn firefox_profile_matches_librewolf_tab() {
        let args = s(&[
            "/usr/lib/librewolf/librewolf",
            "-contentproc",
            "-isForBrowser",
            "tab",
        ]);
        assert!(match_profile(&args, &firefox()));
    }

    #[test]
    fn firefox_profile_matches_zen_browser_tab() {
        let args = s(&[
            "/opt/zen-browser-bin/zen-bin",
            "-contentproc",
            "-isForBrowser",
            "tab",
        ]);
        assert!(match_profile(&args, &firefox()));
    }

    #[test]
    fn firefox_profile_rejects_rdd_process() {
        // rdd has -isForBrowser but ends in "rdd", not "tab"
        let args = s(&[
            "/usr/lib/firefox/firefox",
            "-contentproc",
            "-isForBrowser",
            "rdd",
        ]);
        assert!(!match_profile(&args, &firefox()));
    }

    #[test]
    fn firefox_profile_rejects_main_browser_process() {
        let args = s(&["/usr/lib/firefox/firefox"]);
        assert!(!match_profile(&args, &firefox()));
    }

    #[test]
    fn firefox_profile_rejects_non_firefox_tab() {
        // Some unrelated binary whose argv ends in "tab" — must not match
        let args = s(&["/usr/bin/editor", "-isForBrowser", "tab"]);
        assert!(!match_profile(&args, &firefox()));
    }

    // --- chromium profile ------------------------------------------------

    #[test]
    fn chromium_profile_matches_chrome_renderer() {
        // Real cmdline from a running Chrome content renderer
        let args = s(&[
            "/opt/google/chrome/chrome",
            "--type=renderer",
            "--crashpad-handler-pid=115485",
            "--lang=pt-BR",
            "--num-raster-threads=4",
            "--enable-zero-copy",
        ]);
        assert!(match_profile(&args, &chromium()));
    }

    #[test]
    fn chromium_profile_matches_brave_renderer() {
        let args = s(&[
            "/opt/brave.com/brave/brave",
            "--type=renderer",
            "--lang=en-US",
        ]);
        assert!(match_profile(&args, &chromium()));
    }

    #[test]
    fn chromium_profile_matches_edge_renderer() {
        let args = s(&["/opt/microsoft/msedge/msedge", "--type=renderer"]);
        assert!(match_profile(&args, &chromium()));
    }

    #[test]
    fn chromium_profile_matches_vivaldi_and_opera_and_vesktop() {
        for bin in &[
            "/opt/vivaldi/vivaldi-bin",
            "/usr/lib/opera/opera",
            "/usr/lib/electron40/electron",
            "/usr/share/code/code",
            "/usr/lib/discord/Discord",
            "/usr/lib/slack/slack",
        ] {
            let args = s(&[bin, "--type=renderer"]);
            assert!(
                match_profile(&args, &chromium()),
                "expected {} to match chromium profile",
                bin,
            );
        }
    }

    #[test]
    fn chromium_profile_rejects_extension_renderer() {
        // Real cmdline from a Chrome extension renderer — same --type=renderer
        // but with --extension-process, which we exclude.
        let args = s(&[
            "/opt/google/chrome/chrome",
            "--type=renderer",
            "--extension-process",
            "--lang=pt-BR",
        ]);
        assert!(!match_profile(&args, &chromium()));
    }

    #[test]
    fn chromium_profile_rejects_gpu_process() {
        let args = s(&["/opt/google/chrome/chrome", "--type=gpu-process"]);
        assert!(!match_profile(&args, &chromium()));
    }

    #[test]
    fn chromium_profile_rejects_utility_and_zygote_and_crashpad() {
        for bad_type in &["--type=utility", "--type=zygote", "--type=crashpad-handler"] {
            let args = s(&["/opt/google/chrome/chrome", bad_type]);
            assert!(
                !match_profile(&args, &chromium()),
                "expected chromium profile to reject {}",
                bad_type,
            );
        }
    }

    #[test]
    fn chromium_profile_rejects_main_browser_process() {
        let args = s(&["/opt/google/chrome/chrome"]);
        assert!(!match_profile(&args, &chromium()));
    }

    // --- match_profile general -------------------------------------------

    #[test]
    fn empty_cmdline_never_matches() {
        for p in default_profiles() {
            assert!(!match_profile(&[], &p));
        }
    }

    #[test]
    fn binary_substring_match_is_case_insensitive() {
        let p = BrowserProfile {
            name: "weird".into(),
            binary_substring_any: vec!["FOO".into()],
            arg_required_all: vec![],
            arg_excluded_any: vec![],
            arg_last: None,
        };
        assert!(match_profile(&s(&["/usr/bin/foobar"]), &p));
        assert!(match_profile(&s(&["/usr/bin/FOOBAR"]), &p));
    }
}
