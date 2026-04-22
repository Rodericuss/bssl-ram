use crate::{
    compressor::{compress_pid, rss_mib},
    config::Config,
    state::{eligible_pids, TabReport},
};
use anyhow::Result;
use futures_util::StreamExt;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::accept_async;
use tracing::{error, info, warn};

pub async fn run(config: Config) -> Result<()> {
    let addr: SocketAddr = format!("127.0.0.1:{}", config.ws_port).parse()?;
    let listener = TcpListener::bind(&addr).await?;
    info!("WebSocket server listening on ws://{}", addr);

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("extension connected from {}", peer);
                let cfg = config.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, cfg).await {
                        error!("connection error: {}", e);
                    }
                });
            }
            Err(e) => error!("accept error: {}", e),
        }
    }
}

async fn handle_connection(stream: TcpStream, config: Config) -> Result<()> {
    let ws = accept_async(stream).await?;
    let (_, mut rx) = ws.split();

    while let Some(msg) = rx.next().await {
        let msg = msg?;
        if !msg.is_text() {
            continue;
        }

        let report: TabReport = match serde_json::from_str(msg.to_text()?) {
            Ok(r) => r,
            Err(e) => {
                warn!("invalid message from extension: {}", e);
                continue;
            }
        };

        let pids = eligible_pids(&report, config.idle_threshold_secs);

        for pid in pids {
            let rss = rss_mib(pid);
            if rss < config.min_rss_mib {
                info!("skipping pid {} — RSS {}MiB below threshold {}MiB", pid, rss, config.min_rss_mib);
                continue;
            }

            if let Err(e) = compress_pid(pid, config.dry_run) {
                warn!("failed to compress pid {}: {}", pid, e);
            }
        }
    }

    info!("extension disconnected");
    Ok(())
}
