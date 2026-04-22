//! Native Messaging Host manifest installer.
//!
//! Chrome and Firefox look up NMH manifests by filename inside
//! browser-specific directories. This module writes `io.bssl.ram.json`
//! to every known user-level location so the browser can find us.
//!
//! References:
//!   * Chrome: <https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging>
//!   * Firefox: <https://developer.mozilla.org/en-US/docs/Mozilla/Add-ons/WebExtensions/Native_manifests>

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

const NMH_NAME: &str = "io.bssl.ram";
const NMH_DESCRIPTION: &str = "bssl-ram browser signals bridge";
const FIREFOX_EXT_ID: &str = "bssl-ram-signals@bssl.io";

#[derive(Debug, Serialize)]
struct ChromeManifest<'a> {
    name: &'a str,
    description: &'a str,
    path: String,
    r#type: &'a str,
    allowed_origins: Vec<String>,
}

#[derive(Debug, Serialize)]
struct FirefoxManifest<'a> {
    name: &'a str,
    description: &'a str,
    path: String,
    r#type: &'a str,
    allowed_extensions: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
enum Flavor {
    Chrome,
    Firefox,
}

/// Known NMH manifest directories, relative to `$HOME` when `user`,
/// or absolute when `!user`. The installer tries all of them; missing
/// parents are created with `mkdir -p`.
fn target_dirs(user: bool) -> Vec<(PathBuf, Flavor)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs = Vec::new();

    if user {
        if let Some(h) = home {
            // Chromium-family (user)
            for sub in [
                ".config/google-chrome/NativeMessagingHosts",
                ".config/chromium/NativeMessagingHosts",
                ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts",
                ".config/microsoft-edge/NativeMessagingHosts",
                ".config/opera/NativeMessagingHosts",
                ".config/vivaldi/NativeMessagingHosts",
            ] {
                dirs.push((h.join(sub), Flavor::Chrome));
            }
            // Firefox-family (user)
            dirs.push((h.join(".mozilla/native-messaging-hosts"), Flavor::Firefox));
            dirs.push((h.join(".librewolf/native-messaging-hosts"), Flavor::Firefox));
            dirs.push((h.join(".waterfox/native-messaging-hosts"), Flavor::Firefox));
        }
    } else {
        // System-wide
        dirs.push((
            PathBuf::from("/etc/opt/chrome/native-messaging-hosts"),
            Flavor::Chrome,
        ));
        dirs.push((
            PathBuf::from("/etc/chromium/native-messaging-hosts"),
            Flavor::Chrome,
        ));
        dirs.push((
            PathBuf::from("/usr/lib/mozilla/native-messaging-hosts"),
            Flavor::Firefox,
        ));
        dirs.push((
            PathBuf::from("/usr/lib64/mozilla/native-messaging-hosts"),
            Flavor::Firefox,
        ));
    }

    dirs
}

/// Install NMH manifests pointing at this binary (resolved via
/// `std::env::current_exe`). Returns the list of files written.
pub fn install(user: bool, chrome_ext_id: Option<&str>) -> Result<Vec<PathBuf>> {
    let exe = std::env::current_exe()
        .context("resolving current executable path")?
        .canonicalize()
        .context("canonicalizing executable path")?;
    let exe_str = exe.to_string_lossy().into_owned();

    let chrome_origins = chrome_ext_id
        .map(|id| vec![format!("chrome-extension://{}/", id)])
        .unwrap_or_default();

    let dirs = target_dirs(user);
    let mut written = Vec::new();

    for (dir, flavor) in dirs {
        std::fs::create_dir_all(&dir).with_context(|| format!("mkdir -p {}", dir.display()))?;
        let file = dir.join(format!("{}.json", NMH_NAME));

        match flavor {
            Flavor::Chrome => {
                if chrome_origins.is_empty() {
                    // No ext id supplied — skip Chromium-family dirs
                    // rather than write a broken manifest.
                    continue;
                }
                let manifest = ChromeManifest {
                    name: NMH_NAME,
                    description: NMH_DESCRIPTION,
                    path: exe_str.clone(),
                    r#type: "stdio",
                    allowed_origins: chrome_origins.clone(),
                };
                write_manifest(&file, &manifest)?;
            }
            Flavor::Firefox => {
                let manifest = FirefoxManifest {
                    name: NMH_NAME,
                    description: NMH_DESCRIPTION,
                    path: exe_str.clone(),
                    r#type: "stdio",
                    allowed_extensions: vec![FIREFOX_EXT_ID.into()],
                };
                write_manifest(&file, &manifest)?;
            }
        }
        written.push(file);
    }

    Ok(written)
}

/// Remove every NMH manifest we might have installed. ENOENT is ignored.
pub fn uninstall(user: bool) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for (dir, _flavor) in target_dirs(user) {
        let file = dir.join(format!("{}.json", NMH_NAME));
        match std::fs::remove_file(&file) {
            Ok(()) => removed.push(file),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("removing {}", file.display())),
        }
    }
    Ok(removed)
}

fn write_manifest<M: Serialize>(path: &Path, manifest: &M) -> Result<()> {
    let body = serde_json::to_vec_pretty(manifest).context("serializing NMH manifest")?;
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_manifest_shape() {
        let m = ChromeManifest {
            name: NMH_NAME,
            description: NMH_DESCRIPTION,
            path: "/usr/local/bin/bssl-ram-bridge".into(),
            r#type: "stdio",
            allowed_origins: vec!["chrome-extension://abcdef/".into()],
        };
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["name"], "io.bssl.ram");
        assert_eq!(json["type"], "stdio");
        assert_eq!(json["allowed_origins"][0], "chrome-extension://abcdef/");
    }

    #[test]
    fn firefox_manifest_shape() {
        let m = FirefoxManifest {
            name: NMH_NAME,
            description: NMH_DESCRIPTION,
            path: "/usr/local/bin/bssl-ram-bridge".into(),
            r#type: "stdio",
            allowed_extensions: vec![FIREFOX_EXT_ID.into()],
        };
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["allowed_extensions"][0], "bssl-ram-signals@bssl.io");
    }

    #[test]
    fn user_target_dirs_contain_firefox_and_chrome() {
        std::env::set_var("HOME", "/tmp/fake-home");
        let dirs = target_dirs(true);
        let paths: Vec<_> = dirs
            .iter()
            .map(|(p, _)| p.to_string_lossy().into_owned())
            .collect();
        assert!(
            paths.iter().any(|p| p.contains("google-chrome")),
            "expected Chrome dir in: {:?}",
            paths
        );
        assert!(
            paths.iter().any(|p| p.contains(".mozilla")),
            "expected Firefox dir in: {:?}",
            paths
        );
    }
}
