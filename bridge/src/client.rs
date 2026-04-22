//! Bare HTTP/1.1 client for talking to `bssl-ramd` over the signals
//! Unix socket. Deliberately avoids `reqwest` and any full-stack HTTP
//! client — `hyper::client::conn::http1::handshake` over a
//! `tokio::net::UnixStream` is ~30 lines and pulls no extra deps
//! beyond what the workspace already carries.

use anyhow::{anyhow, Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::client::conn::http1::SendRequest;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

pub struct DaemonClient {
    sender: SendRequest<Full<Bytes>>,
    _conn: JoinHandle<()>,
    closed: bool,
}

impl DaemonClient {
    pub async fn connect(sock_path: &str) -> Result<Self> {
        let stream = UnixStream::connect(sock_path)
            .await
            .with_context(|| format!("connecting to signal UDS at {}", sock_path))?;
        let io = TokioIo::new(stream);
        let (sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
            .await
            .context("hyper handshake over UDS")?;
        let driver = tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(Self {
            sender,
            _conn: driver,
            closed: false,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed || self.sender.is_closed()
    }

    pub async fn get_ping(&mut self) -> Result<serde_json::Value> {
        let req = Request::builder()
            .method("GET")
            .uri("/v1/signals/ping")
            .header("host", "bssl-ram.local")
            .body(Full::new(Bytes::new()))
            .context("building ping request")?;
        let resp = self
            .sender
            .send_request(req)
            .await
            .context("sending ping")
            .inspect_err(|_| self.closed = true)?;
        if !resp.status().is_success() {
            return Err(anyhow!("ping returned status {}", resp.status()));
        }
        let body = resp.into_body().collect().await?.to_bytes();
        serde_json::from_slice(&body).context("parsing ping body")
    }

    pub async fn post_report(
        &mut self,
        payload: &serde_json::Value,
        ext_id: &str,
    ) -> Result<(bool, u16)> {
        let body = serde_json::to_vec(payload).context("serializing report payload")?;
        let req = Request::builder()
            .method("POST")
            .uri("/v1/signals/report")
            .header("host", "bssl-ram.local")
            .header("content-type", "application/json")
            .header("x-bssl-extension-id", ext_id)
            .body(Full::new(Bytes::from(body)))
            .context("building report request")?;
        let resp = self
            .sender
            .send_request(req)
            .await
            .context("sending report")
            .inspect_err(|_| self.closed = true)?;
        let status = resp.status();
        // Drain body so hyper is happy to re-use the connection.
        let _ = resp.into_body().collect().await;
        Ok((status.is_success(), status.as_u16()))
    }
}
