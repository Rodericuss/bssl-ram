use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
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

// Intentionally not `Default` — a zero-Duration TTL would make every
// report look stale immediately and silently neuter the feature.
// Construct explicitly via `SignalStore::new(ttl, interaction_grace)`.
#[derive(Debug)]
pub struct SignalStore {
    ttl: Duration,
    interaction_grace: Duration,
    reports: RwLock<HashMap<String, CachedReport>>,
}

impl SignalStore {
    pub fn new(ttl: Duration, interaction_grace: Duration) -> Self {
        Self {
            ttl,
            interaction_grace,
            reports: RwLock::new(HashMap::new()),
        }
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

pub async fn spawn_server(
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
    let router = Router::new()
        .route("/v1/signals/ping", get(ping_handler))
        .route("/v1/signals/report", post(report_handler))
        .layer(DefaultBodyLimit::max(MAX_REPORT_BYTES))
        .with_state(store.clone());

    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, router).await {
            warn!(err = %err, "browser signal server stopped unexpectedly");
        }
    });

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
    headers: HeaderMap,
    Json(report): Json<BrowserSignalsReport>,
) -> StatusCode {
    if headers.get(SOURCE_HEADER).and_then(|v| v.to_str().ok()) != Some(SOURCE_VALUE) {
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
}
