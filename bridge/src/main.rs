//! bssl-ram-bridge — Native Messaging Host for the bssl-ram signals
//! feature.
//!
//! The bridge is a tiny `stdio` process that the browser spawns when
//! the extension calls `chrome.runtime.connectNative("io.bssl.ram")`.
//! It shuttles framed JSON between stdin/stdout (the browser) and a
//! Unix domain socket (`bssl-ramd`).
//!
//! Protocol on stdin/stdout: 4-byte native-endian uint32 length header
//! + UTF-8 JSON body. Protocol on the UDS: bare HTTP/1.1 to
//!   `/v1/signals/report` and `/v1/signals/ping` on the daemon side.
//!
//! Every decision is logged to stderr (captured by the browser's
//! devtools). No stdout outside the frame protocol — stdout IS the
//! wire.

mod client;
mod install;
mod nmh;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::io::BufReader;
use tracing::{debug, info, warn};

const DEFAULT_SOCK: &str = "/run/bssl-ram/signals.sock";

#[derive(Parser, Debug)]
#[command(
    name = "bssl-ram-bridge",
    version,
    about = "Native Messaging Host bridge between browser extensions and bssl-ramd"
)]
struct Cli {
    /// First positional — set by the browser when it spawns us.
    /// Chrome passes `chrome-extension://<id>/`, Firefox passes the
    /// extension ID. Absent on `install`/`uninstall` subcommand runs.
    origin: Option<String>,

    #[command(subcommand)]
    command: Option<SubCmd>,
}

#[derive(Subcommand, Debug)]
enum SubCmd {
    /// Write the NMH manifest to per-user browser directories.
    Install {
        /// Install to `$HOME/.config/.../NativeMessagingHosts/` and
        /// `$HOME/.mozilla/native-messaging-hosts/` (default).
        #[arg(long, default_value_t = true)]
        user: bool,
        /// Chrome extension ID — required for Chromium-family manifests.
        /// Firefox uses the fixed extension ID from the manifest.
        #[arg(long)]
        chrome_ext_id: Option<String>,
    },
    /// Remove every NMH manifest this binary might have written.
    Uninstall {
        #[arg(long, default_value_t = true)]
        user: bool,
    },
}

fn init_tracing() {
    // Everything goes to stderr — stdout is the NMH wire.
    let env = tracing_subscriber::EnvFilter::try_from_env("BSSL_BRIDGE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(env)
        .with_writer(std::io::stderr)
        .compact()
        .init();
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Some(SubCmd::Install {
                 user,
                 chrome_ext_id,
             }) => {
            let written = install::install(user, chrome_ext_id.as_deref())?;
            for path in &written {
                println!("installed {}", path.display());
            }
            if chrome_ext_id.is_none() {
                eprintln!(
                    "note: --chrome-ext-id not supplied → Chromium-family manifests skipped. \
                     Firefox manifests were still written."
                );
            }
            return Ok(());
        }
        Some(SubCmd::Uninstall { user }) => {
            let removed = install::uninstall(user)?;
            for path in &removed {
                println!("removed {}", path.display());
            }
            return Ok(());
        }
        None => {}
    }

    run_bridge(cli.origin.unwrap_or_default()).await
}

async fn run_bridge(origin: String) -> Result<()> {
    info!(origin = %origin, "bssl-ram-bridge starting");

    let sock_path = std::env::var("BSSL_RAM_SOCK").unwrap_or_else(|_| DEFAULT_SOCK.to_string());

    let mut client = match client::DaemonClient::connect(&sock_path).await {
        Ok(c) => c,
        Err(err) => {
            warn!(err = %err, "daemon UDS unreachable — emitting single error frame and exiting");
            let mut stdout = tokio::io::stdout();
            let _ = nmh::write_frame(
                &mut stdout,
                &serde_json::json!({"ok": false, "reason": "daemon unreachable"}),
            )
                .await;
            return Ok(());
        }
    };

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = sigterm.recv() => {
                info!("SIGTERM received — exiting cleanly");
                break;
            }
            res = nmh::read_frame(&mut stdin) => {
                let frame = match res {
                    Ok(v) => v,
                    Err(err) => {
                        // EOF = browser closed the port / is shutting
                        // us down. Not an error worth a stacktrace.
                        debug!(err = %err, "stdin closed");
                        break;
                    }
                };

                if client.is_closed() {
                    // Daemon went away mid-session — reply once and
                    // let the browser respawn us on next postMessage.
                    let _ = nmh::write_frame(&mut stdout,
                        &serde_json::json!({"ok": false, "reason": "daemon disconnected"})
                    ).await;
                    break;
                }

                handle_frame(frame, &mut client, &origin, &mut stdout).await;
            }
        }
    }

    info!("bssl-ram-bridge stopped");
    Ok(())
}

async fn handle_frame(
    frame: serde_json::Value,
    client: &mut client::DaemonClient,
    origin: &str,
    stdout: &mut tokio::io::Stdout,
) {
    let kind = frame.get("kind").and_then(|v| v.as_str()).unwrap_or("");

    let response = match kind {
        "ping" => match client.get_ping().await {
            Ok(mut body) => {
                // Augment with our own identity so the extension can
                // display the bridge version on the options page.
                body["bridge_version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
                body["bridge_kind"] = serde_json::json!("native-messaging-uds");
                body
            }
            Err(err) => {
                warn!(err = %err, "daemon ping failed");
                serde_json::json!({"ok": false, "reason": format!("ping failed: {}", err)})
            }
        },
        "report" => {
            let empty = serde_json::json!(null);
            let payload = frame.get("payload").unwrap_or(&empty);
            match client.post_report(payload, origin).await {
                Ok((true, _)) => serde_json::json!({"ok": true}),
                Ok((false, status)) => {
                    warn!(status, "daemon rejected report");
                    serde_json::json!({"ok": false, "status": status})
                }
                Err(err) => {
                    warn!(err = %err, "daemon report forward failed");
                    serde_json::json!({"ok": false, "reason": format!("{}", err)})
                }
            }
        }
        other => {
            warn!(kind = other, "unknown frame kind from extension");
            serde_json::json!({"ok": false, "reason": "unknown kind"})
        }
    };

    if let Err(err) = nmh::write_frame(stdout, &response).await {
        warn!(err = %err, "writing response frame to stdout failed");
    }
}
