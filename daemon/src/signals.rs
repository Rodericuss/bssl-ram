use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as HyperAutoBuilder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tower::Service;
use tracing::{debug, info, warn};

const SOURCE_HEADER: &str = "x-bssl-signal-source";
const SOURCE_VALUE: &str = "extension";

/// Wire protocol version advertised by the daemon. The extension must
/// send reports whose `protocol_version` matches this value or the
/// daemon rejects them. Bump on breaking payload changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Max payload size for `/v1/signals/report`. A session with ~1000 tabs
/// serializes to ~300 KiB; 1 MiB gives us comfortable headroom before
/// axum returns 413 to a legitimate browser.
const MAX_REPORT_BYTES: usize = 1024 * 1024;

/// Maximum skew between the report's self-declared `sent_at_ms` and
/// the daemon's wall clock before we reject it as stale/forged.
/// Set to 2× the default TTL so a report that crossed the wire slowly
/// but is still inside TTL is accepted.
const SENT_AT_MAX_SKEW_MS: u64 = 120_000;

#[derive(Debug, Clone)]
pub struct ProfileVeto {
    pub reason: &'static str,
    pub detail: String,
}

#[derive(Debug, Clone)]
struct CachedReport {
    received_at: Instant,
    report: BrowserSignalsReport,
}

/// Owns the socket file path so that on daemon shutdown (the last
/// `Arc<SignalStore>` being dropped) we unlink it. systemd
/// `RuntimeDirectory=` already handles the happy path; this catches
/// `cargo run` and `SIGTERM` from outside the unit.
#[derive(Debug)]
struct SocketCleanup {
    path: std::path::PathBuf,
}

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// Intentionally not `Default` — a zero-Duration TTL would make every
// report look stale immediately and silently neuter the feature.
// Construct explicitly via `SignalStore::new(ttl, interaction_grace)`.
#[derive(Debug)]
pub struct SignalStore {
    ttl: Duration,
    interaction_grace: Duration,
    reports: RwLock<HashMap<String, CachedReport>>,
    // Held for the lifetime of the store so `Drop` unlinks the UDS
    // socket file on daemon shutdown. `None` for TCP transport.
    _socket_cleanup: Option<SocketCleanup>,
}

impl SignalStore {
    pub fn new(ttl: Duration, interaction_grace: Duration) -> Self {
        Self {
            ttl,
            interaction_grace,
            reports: RwLock::new(HashMap::new()),
            _socket_cleanup: None,
        }
    }

    fn with_cleanup(mut self, path: std::path::PathBuf) -> Self {
        self._socket_cleanup = Some(SocketCleanup { path });
        self
    }

    fn apply_report(&self, report: BrowserSignalsReport) -> Result<()> {
        self.apply_report_at(wall_clock_ms(), report)
    }

    fn apply_report_at(&self, now_wall: u64, mut report: BrowserSignalsReport) -> Result<()> {
        // Reject reports from the future or from well before our TTL
        // window. Protects against a hibernated service worker flushing
        // a stale batch after the browser has been unreachable.
        let skew_ms = now_wall.abs_diff(report.sent_at_ms);
        if skew_ms > SENT_AT_MAX_SKEW_MS {
            anyhow::bail!(
                "sent_at_ms skew {}ms exceeds limit {}ms",
                skew_ms,
                SENT_AT_MAX_SKEW_MS
            );
        }

        let family = canonical_family(&report.browser.family)
            .context("browser.family missing or unsupported")?;
        report.browser.family = family.clone();

        let tabs = report.tabs.len();
        let audible_tabs = report.tabs.iter().filter(|tab| tab.audible).count();
        let focused_windows = report.tabs.iter().filter(|tab| tab.window_focused).count();

        let mut guard = self
            .reports
            .write()
            .expect("signal report store write lock poisoned");
        guard.insert(
            family.clone(),
            CachedReport {
                received_at: Instant::now(),
                report,
            },
        );

        info!(
            family,
            tabs, audible_tabs, focused_windows, "browser signal report accepted"
        );
        Ok(())
    }

    pub fn profile_veto(&self, profile: &str) -> Option<ProfileVeto> {
        self.profile_veto_at(profile, Instant::now(), wall_clock_ms())
    }

    fn profile_veto_at(
        &self,
        profile: &str,
        now: Instant,
        now_wall_ms: u64,
    ) -> Option<ProfileVeto> {
        let family = canonical_family(profile)?;

        let mut guard = self
            .reports
            .write()
            .expect("signal report store write lock poisoned");

        guard.retain(|_, cached| now.duration_since(cached.received_at) <= self.ttl);
        let cached = guard.get(&family)?;
        let report = &cached.report;

        let audible_tabs = report.tabs.iter().filter(|tab| tab.audible).count();
        if audible_tabs > 0 {
            return Some(ProfileVeto {
                reason: "audible-tab",
                detail: format!("{} audible tab(s) reported", audible_tabs),
            });
        }

        let playing_media = report
            .tabs
            .iter()
            .filter_map(|tab| tab.content.as_ref())
            .map(|content| content.playing_media_elements)
            .sum::<u32>();
        if playing_media > 0 {
            return Some(ProfileVeto {
                reason: "playing-media",
                detail: format!("{} media element(s) actively playing", playing_media),
            });
        }

        let focused_windows = report.tabs.iter().filter(|tab| tab.window_focused).count();
        if focused_windows > 0 {
            return Some(ProfileVeto {
                reason: "focused-window",
                detail: format!("{} focused browser window(s) reported", focused_windows),
            });
        }

        let recent_cutoff = now_wall_ms.saturating_sub(self.interaction_grace.as_millis() as u64);
        let recent_interactions = report
            .tabs
            .iter()
            .filter_map(|tab| tab.content.as_ref())
            .filter_map(|content| content.last_user_interaction_ms)
            .filter(|ts| *ts >= recent_cutoff)
            .count();
        if recent_interactions > 0 {
            return Some(ProfileVeto {
                reason: "recent-interaction",
                detail: format!(
                    "{} tab(s) reported user interaction within the last {}s",
                    recent_interactions,
                    self.interaction_grace.as_secs()
                ),
            });
        }

        None
    }
}

/// Marker inserted into the request extensions telling `report_handler`
/// whether the `x-bssl-signal-source` header check should apply. On UDS
/// the `SO_PEERCRED` uid assertion is the real trust boundary; the
/// header check is meaningful only on TCP (defense-in-depth marker).
#[derive(Clone, Copy)]
struct TransportKind {
    requires_source_header: bool,
}

fn build_router(store: Arc<SignalStore>, requires_source_header: bool) -> Router {
    Router::new()
        .route("/v1/signals/ping", get(ping_handler))
        .route("/v1/signals/report", post(report_handler))
        .layer(DefaultBodyLimit::max(MAX_REPORT_BYTES))
        .layer(Extension(TransportKind {
            requires_source_header,
        }))
        .with_state(store)
}

/// Dispatcher. `transport` = "uds" (default) | "tcp". `target` is the
/// socket path for UDS, the `host:port` bind for TCP.
pub async fn spawn_server(
    transport: &str,
    target: &str,
    ttl: Duration,
    interaction_grace: Duration,
) -> Result<Arc<SignalStore>> {
    match transport {
        "uds" => spawn_uds_server(target, ttl, interaction_grace).await,
        "tcp" => spawn_tcp_server(target, ttl, interaction_grace).await,
        other => anyhow::bail!(
            "unknown signal_transport {:?} (expected \"uds\" or \"tcp\")",
            other
        ),
    }
}

async fn spawn_tcp_server(
    bind: &str,
    ttl: Duration,
    interaction_grace: Duration,
) -> Result<Arc<SignalStore>> {
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("parsing signal_server_bind {:?}", bind))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding browser signal server to {}", bind))?;

    let store = Arc::new(SignalStore::new(ttl, interaction_grace));
    let router = build_router(store.clone(), /* requires_source_header = */ true);

    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            warn!(err = %err, "browser signal server stopped unexpectedly");
        }
    });

    info!(bind, "browser signal server bound (tcp)");
    Ok(store)
}

async fn spawn_uds_server(
    sock_path: &str,
    ttl: Duration,
    interaction_grace: Duration,
) -> Result<Arc<SignalStore>> {
    // Remove any stale socket from a previous run that did not clean up
    // (crash, SIGKILL, systemd without RuntimeDirectory). `UnixListener::bind`
    // returns EADDRINUSE otherwise.
    let _ = std::fs::remove_file(sock_path);

    let listener = tokio::net::UnixListener::bind(sock_path)
        .with_context(|| format!("binding signal UDS at {}", sock_path))?;

    // Chmod immediately after bind — axum/hyper-util does not do this,
    // and a stray 0755 socket would leak the surface to any local user.
    std::fs::set_permissions(sock_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", sock_path))?;

    let store = Arc::new(
        SignalStore::new(ttl, interaction_grace).with_cleanup(std::path::PathBuf::from(sock_path)),
    );
    let router = build_router(store.clone(), /* requires_source_header = */ false);
    let our_uid: u32 = unsafe { libc::geteuid() };

    let sock_display = sock_path.to_string();
    tokio::spawn(async move {
        loop {
            let (stream, _addr) = match listener.accept().await {
                Ok(pair) => pair,
                Err(err) => {
                    warn!(err = %err, "UDS accept failed");
                    continue;
                }
            };

            // Trust boundary: reject if the peer UID is not ours. The
            // daemon and the native-messaging bridge must both run as
            // the same user (the systemd unit binds to User=%i).
            match stream.peer_cred() {
                Ok(cred) if cred.uid() == our_uid => {}
                Ok(cred) => {
                    warn!(
                        peer_uid = cred.uid(),
                        our_uid, "UDS peer UID mismatch — dropping connection"
                    );
                    continue;
                }
                Err(err) => {
                    warn!(err = %err, "UDS peer_cred() failed — dropping connection");
                    continue;
                }
            }

            // Turn the router into a service for this single connection.
            let mut make_service = router.clone().into_make_service();
            let svc = match make_service.call(&()).await {
                Ok(svc) => svc,
                Err(err) => {
                    warn!(err = %err, "signal router into_service failed");
                    continue;
                }
            };

            let svc = hyper_util::service::TowerToHyperService::new(svc);
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                if let Err(err) = HyperAutoBuilder::new(TokioExecutor::new())
                    .serve_connection(io, svc)
                    .await
                {
                    debug!(err = %err, "UDS HTTP connection closed with error");
                }
            });
        }
    });

    info!(path = %sock_display, "browser signal server bound (uds)");
    Ok(store)
}

/// Lightweight handshake endpoint. The extension hits this on startup
/// to (a) confirm the daemon is reachable and (b) verify both sides
/// agree on `protocol_version` before any real report is built.
#[derive(Debug, Serialize)]
struct PingResponse {
    protocol_version: u32,
    accepted_families: &'static [&'static str],
    max_report_bytes: usize,
}

async fn ping_handler() -> Json<PingResponse> {
    Json(PingResponse {
        protocol_version: PROTOCOL_VERSION,
        accepted_families: &["firefox", "chromium"],
        max_report_bytes: MAX_REPORT_BYTES,
    })
}

async fn report_handler(
    State(store): State<Arc<SignalStore>>,
    Extension(transport): Extension<TransportKind>,
    headers: HeaderMap,
    Json(report): Json<BrowserSignalsReport>,
) -> StatusCode {
    if transport.requires_source_header
        && headers.get(SOURCE_HEADER).and_then(|v| v.to_str().ok()) != Some(SOURCE_VALUE)
    {
        debug!("rejecting browser signal report: missing trusted source header");
        return StatusCode::UNAUTHORIZED;
    }

    // Accept either the legacy `version` field or the new
    // `protocol_version` field, but require at least one to match.
    // `version` is kept for a couple of releases for extension rollouts
    // that haven't been rebuilt yet.
    let wire_version = if report.protocol_version != 0 {
        report.protocol_version
    } else {
        report.version
    };
    if wire_version != PROTOCOL_VERSION {
        debug!(
            version = report.version,
            protocol_version = report.protocol_version,
            "rejecting browser signal report: unsupported protocol version"
        );
        return StatusCode::BAD_REQUEST;
    }

    match store.apply_report(report) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(err) => {
            debug!(err = %err, "rejecting browser signal report: invalid payload");
            StatusCode::BAD_REQUEST
        }
    }
}

fn canonical_family(raw: &str) -> Option<String> {
    match raw.to_ascii_lowercase().as_str() {
        "firefox" | "librewolf" | "waterfox" | "icecat" | "zen" => Some("firefox".into()),
        "chromium" | "chrome" | "brave" | "edge" | "opera" | "vivaldi" | "thorium" => {
            Some("chromium".into())
        }
        _ => None,
    }
}

fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct BrowserSignalsReport {
    /// Legacy field kept for one transition window. New extensions
    /// must populate `protocol_version` instead. If both are present
    /// `protocol_version` wins.
    version: u32,
    /// Wire protocol version. Zero means the extension is old and the
    /// daemon falls back to `version` above.
    protocol_version: u32,
    /// Extension-provided hint for how long this report is
    /// authoritative. When zero the daemon uses its own `signal_ttl`.
    /// Bounded on the read side so a buggy/malicious value can't pin
    /// a veto forever.
    veto_ttl_ms: u64,
    sent_at_ms: u64,
    browser: BrowserDescriptor,
    summary: SignalSummary,
    tabs: Vec<TabSignal>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct BrowserDescriptor {
    family: String,
    /// UUID v4 the extension persists in `storage.local` on first run.
    /// Lets the daemon distinguish two concurrent profiles of the same
    /// family (e.g. two Firefox installs) and purge stale state when a
    /// profile disappears.
    instance_id: String,
    system_idle_state: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SignalSummary {
    tabs_total: u32,
    audible_tabs: u32,
    hidden_tabs: u32,
    discarded_tabs: u32,
    active_tabs: u32,
    focused_windows: u32,
    content_samples: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct TabSignal {
    tab_id: i64,
    window_id: i64,
    active: bool,
    audible: bool,
    hidden: bool,
    discarded: bool,
    last_accessed_ms: Option<u64>,
    window_focused: bool,
    url_origin: Option<String>,
    content: Option<ContentSignal>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ContentSignal {
    visibility_state: String,
    document_has_focus: bool,
    fullscreen: bool,
    picture_in_picture: bool,
    media_elements: u32,
    playing_media_elements: u32,
    last_user_interaction_ms: Option<u64>,
    sampled_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic wall-clock used across the suite. Keeps `sent_at_ms`
    /// and the `now_wall_ms` passed to `profile_veto_at` aligned, so
    /// the anti-stale check in `apply_report_at` is happy.
    const FAKE_NOW_MS: u64 = 10_000;

    fn report(family: &str) -> BrowserSignalsReport {
        BrowserSignalsReport {
            version: 1,
            protocol_version: PROTOCOL_VERSION,
            veto_ttl_ms: 0,
            sent_at_ms: FAKE_NOW_MS,
            browser: BrowserDescriptor {
                family: family.into(),
                instance_id: "test-instance".into(),
                system_idle_state: "active".into(),
            },
            summary: SignalSummary::default(),
            tabs: Vec::new(),
        }
    }

    #[test]
    fn canonicalizes_report_family() {
        let store = SignalStore::new(Duration::from_secs(45), Duration::from_secs(90));
        store.apply_report_at(FAKE_NOW_MS, report("edge")).unwrap();
        assert!(store
            .profile_veto_at("chromium", Instant::now(), FAKE_NOW_MS)
            .is_none());
    }

    #[test]
    fn audible_tab_veto_wins_first() {
        let store = SignalStore::new(Duration::from_secs(45), Duration::from_secs(90));
        let mut payload = report("firefox");
        payload.tabs.push(TabSignal {
            audible: true,
            ..TabSignal::default()
        });
        store.apply_report_at(FAKE_NOW_MS, payload).unwrap();

        let veto = store
            .profile_veto_at("firefox", Instant::now(), FAKE_NOW_MS)
            .expect("expected veto");
        assert_eq!(veto.reason, "audible-tab");
    }

    #[test]
    fn focused_window_blocks_profile() {
        let store = SignalStore::new(Duration::from_secs(45), Duration::from_secs(90));
        let mut payload = report("firefox");
        payload.tabs.push(TabSignal {
            window_focused: true,
            ..TabSignal::default()
        });
        store.apply_report_at(FAKE_NOW_MS, payload).unwrap();

        let veto = store
            .profile_veto_at("firefox", Instant::now(), FAKE_NOW_MS)
            .expect("expected veto");
        assert_eq!(veto.reason, "focused-window");
    }

    #[test]
    fn recent_interaction_uses_wall_clock_grace() {
        let store = SignalStore::new(Duration::from_secs(45), Duration::from_secs(90));
        let mut payload = report("firefox");
        payload.tabs.push(TabSignal {
            content: Some(ContentSignal {
                last_user_interaction_ms: Some(FAKE_NOW_MS - 50),
                ..ContentSignal::default()
            }),
            ..TabSignal::default()
        });
        store.apply_report_at(FAKE_NOW_MS, payload).unwrap();

        let veto = store
            .profile_veto_at("firefox", Instant::now(), FAKE_NOW_MS)
            .expect("expected veto");
        assert_eq!(veto.reason, "recent-interaction");
    }

    #[test]
    fn stale_reports_are_ignored() {
        let store = SignalStore::new(Duration::from_secs(1), Duration::from_secs(90));
        let mut payload = report("firefox");
        payload.tabs.push(TabSignal {
            audible: true,
            ..TabSignal::default()
        });
        store.apply_report_at(FAKE_NOW_MS, payload).unwrap();

        let veto = store.profile_veto_at(
            "firefox",
            Instant::now() + Duration::from_secs(2),
            FAKE_NOW_MS,
        );
        assert!(veto.is_none());
    }

    #[test]
    fn rejects_excessive_sent_at_skew() {
        let store = SignalStore::new(Duration::from_secs(45), Duration::from_secs(90));
        let mut payload = report("firefox");
        // Push sent_at_ms far into the future relative to the synthetic
        // "now" we hand to apply_report_at → must bail.
        payload.sent_at_ms = FAKE_NOW_MS + SENT_AT_MAX_SKEW_MS + 1_000;
        let err = store.apply_report_at(FAKE_NOW_MS, payload).unwrap_err();
        assert!(err.to_string().contains("skew"));
    }

    #[test]
    fn rejects_unknown_family() {
        let store = SignalStore::new(Duration::from_secs(45), Duration::from_secs(90));
        let err = store
            .apply_report_at(FAKE_NOW_MS, report("netscape-navigator"))
            .unwrap_err();
        assert!(err.to_string().contains("unsupported"));
    }

    #[test]
    fn instance_id_round_trips() {
        let store = SignalStore::new(Duration::from_secs(45), Duration::from_secs(90));
        let mut payload = report("firefox");
        payload.browser.instance_id = "abc-123".into();
        store.apply_report_at(FAKE_NOW_MS, payload).unwrap();
        // Veto lookup is family-keyed, but the cached report retains
        // instance_id for future per-instance routing.
        let guard = store.reports.read().unwrap();
        assert_eq!(
            guard.get("firefox").unwrap().report.browser.instance_id,
            "abc-123"
        );
    }

    /// End-to-end UDS test — binds the real server on a tempdir path,
    /// posts a minimal valid report via a raw `UnixStream` + hyper
    /// handshake, and asserts 204.
    ///
    /// The peer-UID mismatch path is deliberately *not* covered here:
    /// reproducing it in-process would require changing our EUID,
    /// which a standard `cargo test` cannot do. The branch is a
    /// single `if peer_uid != our_uid` guard in `spawn_uds_server`
    /// and is exercised by integration tests run as a second UID.
    #[tokio::test]
    async fn uds_server_accepts_report_with_correct_peer_uid() {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;

        let tmp = tempfile::tempdir().unwrap();
        let sock_path = tmp.path().join("signals.sock");
        let sock_str = sock_path.to_str().unwrap();

        let store = spawn_uds_server(sock_str, Duration::from_secs(45), Duration::from_secs(90))
            .await
            .expect("spawn_uds_server");

        // Give the accept loop a moment to enter listen state.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let stream = tokio::net::UnixStream::connect(&sock_path)
            .await
            .expect("connect to uds");
        let io = TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(io)
            .await
            .expect("hyper handshake");
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let body = serde_json::to_vec(&serde_json::json!({
            "protocol_version": 1,
            "sent_at_ms": wall_clock_ms(),
            "browser": { "family": "firefox", "instance_id": "test" },
            "tabs": [{ "audible": true }]
        }))
            .unwrap();

        let req = hyper::Request::builder()
            .method("POST")
            .uri("/v1/signals/report")
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .unwrap();

        let resp = sender.send_request(req).await.expect("send_request");
        assert_eq!(resp.status(), 204);
        // Drain
        let _ = resp.into_body().collect().await;

        // Sanity-check the audible veto reached the store through UDS.
        let veto = store
            .profile_veto_at("firefox", Instant::now(), wall_clock_ms())
            .expect("audible veto should be set");
        assert_eq!(veto.reason, "audible-tab");

        // The listener task holds its own clone of Arc<SignalStore>,
        // so dropping our store handle here does NOT trigger the
        // SocketCleanup Drop in-test; the cleanup path is exercised at
        // process exit. `tempfile::TempDir` removes the parent dir on
        // drop either way, so we don't leak filesystem state.
        drop(store);
        assert!(
            sock_path.exists(),
            "socket stays alive while the accept loop holds its clone"
        );
    }

    #[test]
    fn socket_cleanup_unlinks_on_drop() {
        // Narrow unit test for the SocketCleanup Drop contract — the
        // integration test above can't exercise it because the spawned
        // accept-loop task keeps a SignalStore clone alive. Here we
        // build a standalone SocketCleanup and prove the Drop removes
        // the file synchronously.
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("phantom.sock");
        std::fs::write(&sock, b"not-actually-a-socket").unwrap();
        assert!(sock.exists());
        {
            let _guard = SocketCleanup { path: sock.clone() };
        } // guard drops here
        assert!(!sock.exists(), "SocketCleanup must unlink on Drop");
    }

    #[tokio::test]
    async fn uds_server_rejects_unknown_transport() {
        let err = spawn_server(
            "doesnotexist",
            "ignored",
            Duration::from_secs(45),
            Duration::from_secs(90),
        )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown signal_transport"));
    }
}
