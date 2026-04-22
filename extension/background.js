// bssl-ram signals — background service worker
//
// Notes on Manifest V3 behavior we MUST design around:
//
//   1. The SW is shut down after ~30s of idle. Every top-level `let`/`const`
//      is re-initialized on wake. Only `chrome.storage.*` survives. We
//      rehydrate transient state on every top-level run.
//
//   2. `setTimeout` / `setInterval` do NOT keep the SW alive. Use
//      `chrome.alarms` for anything that needs to fire after a delay.
//      Minimum `delayInMinutes` is 1/60 (1s) unpacked, 0.5 (30s) in
//      production stable Chrome — our debounce gracefully degrades to
//      whichever floor the runtime enforces.
//
//   3. `fetch()` in-flight does NOT keep the SW alive either. A report
//      can be aborted mid-flight; next tick / alarm will retry.
//
//   4. `Map` / `Set` do NOT serialize into `chrome.storage.*` — they
//      round-trip as empty objects. We convert to arrays at the
//      boundary.

const api = globalThis.browser ?? globalThis.chrome;

const DAEMON_PROTOCOL_VERSION = 1;

const REPORT_ALARM_TICK = "bssl-ram-report-tick";
const REPORT_ALARM_DEBOUNCE = "bssl-ram-report-debounce";
const REPORT_PERIOD_MINUTES = 0.5;       // safety-net cadence (30s in prod)
const REPORT_DEBOUNCE_MINUTES = 1 / 60;  // event-driven debounce (≥1s unpacked, ≥30s prod)

const CONTENT_TTL_MS = 30_000;
const STORAGE_KEY_TAB_SIGNALS = "tabSignals";
const STORAGE_KEY_INSTANCE_ID = "instanceId";
const STORAGE_KEY_LAST_FAILURE_AT = "lastFailureAt";

// Identifier for the rich-signals content script. Only registered when
// the user has granted `<all_urls>` at the options page. Without it we
// still deliver coarse signals (tab.audible, tab.active, window.focused).
const RICH_SCRIPT_ID = "bssl-ram-rich-signals";

// Name the browser uses to look up the NMH manifest `io.bssl.ram.json`
// under `~/.config/<browser>/NativeMessagingHosts/` or
// `~/.mozilla/native-messaging-hosts/`. Must match what
// `bssl-ram-bridge install` writes.
const NMH_HOST = "io.bssl.ram";

// ---------------------------------------------------------------------------
// Persistent state helpers
// ---------------------------------------------------------------------------

// tabSignals is kept in-memory for fast access and mirrored into
// chrome.storage.session so it survives SW hibernation (session storage
// dies only on browser restart, which is what we want — a fresh browser
// session has no stale content signals to report).
const tabSignals = new Map();

async function hydrateTabSignals() {
    try {
        const stored = await api.storage.session.get(STORAGE_KEY_TAB_SIGNALS);
        const entries = stored?.[STORAGE_KEY_TAB_SIGNALS];
        if (Array.isArray(entries)) {
            for (const [rawId, signal] of entries) {
                const id = Number(rawId);
                if (Number.isFinite(id) && signal && typeof signal === "object") {
                    tabSignals.set(id, signal);
                }
            }
        }
    } catch (_) {
        // session storage may be unavailable very early — not fatal.
    }
}

async function persistTabSignals() {
    try {
        await api.storage.session.set({
            [STORAGE_KEY_TAB_SIGNALS]: Array.from(tabSignals.entries())
        });
    } catch (_) {
        // Quota exhaustion / session unavailable — the in-memory copy
        // still works for this SW lifetime; next wake rebuilds from
        // content-script retries.
    }
}

async function getOrCreateInstanceId() {
    const existing = await api.storage.local.get(STORAGE_KEY_INSTANCE_ID);
    const cached = existing?.[STORAGE_KEY_INSTANCE_ID];
    if (typeof cached === "string" && cached.length > 0) {
        return cached;
    }

    // `crypto.randomUUID` is available in MV3 service workers on Chrome
    // 92+ and Firefox 95+. Both of those floors are well below our
    // manifest's `minimum_chrome_version` / `strict_min_version`.
    const fresh = globalThis.crypto.randomUUID();
    await api.storage.local.set({[STORAGE_KEY_INSTANCE_ID]: fresh});
    return fresh;
}

// ---------------------------------------------------------------------------
// Report scheduling (SW-safe — chrome.alarms only, no setTimeout)
// ---------------------------------------------------------------------------

async function scheduleReportSoon() {
    // Debounce: always recreate with the same name — last one wins.
    // Floor of 1/60 min (1s) in unpacked, 0.5 min (30s) in production
    // stable Chrome. Either way, the periodic safety-net catches us.
    try {
        await api.alarms.create(REPORT_ALARM_DEBOUNCE, {
            delayInMinutes: REPORT_DEBOUNCE_MINUTES
        });
    } catch (_) {
        // If alarm creation fails we still have the periodic tick.
    }
}

// ---------------------------------------------------------------------------
// Browser family detection (cached in local storage to avoid repeat work)
// ---------------------------------------------------------------------------

async function detectBrowserFamily() {
    try {
        if (typeof api.runtime.getBrowserInfo === "function") {
            const info = await api.runtime.getBrowserInfo();
            const name = info?.name?.toLowerCase() ?? "";
            if (name.includes("firefox")) return "firefox";
            if (name.includes("librewolf") || name.includes("waterfox") || name.includes("zen")) {
                return "firefox";
            }
            if (name.includes("edge") || name.includes("opera") || name.includes("chrom") || name.includes("brave")) {
                return "chromium";
            }
            return name || "unknown";
        }
    } catch (_) {
        /* fall through to UA sniffing */
    }

    const ua = (globalThis.navigator?.userAgent ?? "").toLowerCase();
    if (ua.includes("firefox")) return "firefox";
    if (ua.includes("edg/") || ua.includes("opr/") || ua.includes("chrome")) {
        return "chromium";
    }
    return "unknown";
}

// ---------------------------------------------------------------------------
// Report building
// ---------------------------------------------------------------------------

function safeOrigin(rawUrl) {
    if (!rawUrl) return null;
    try {
        const url = new URL(rawUrl);
        if (url.protocol === "http:" || url.protocol === "https:") {
            return url.origin;
        }
    } catch (_) {
        return null;
    }
    return null;
}

function normalizeContentSignal(message) {
    return {
        visibility_state: message.visibility_state ?? "unknown",
        document_has_focus: Boolean(message.document_has_focus),
        fullscreen: Boolean(message.fullscreen),
        picture_in_picture: Boolean(message.picture_in_picture),
        media_elements: Number.isFinite(message.media_elements) ? message.media_elements : 0,
        playing_media_elements: Number.isFinite(message.playing_media_elements)
            ? message.playing_media_elements
            : 0,
        last_user_interaction_ms: Number.isFinite(message.last_user_interaction_ms)
            ? message.last_user_interaction_ms
            : null,
        sampled_at_ms: Number.isFinite(message.sampled_at_ms) ? message.sampled_at_ms : Date.now()
    };
}

function trimExpiredSignals(now) {
    for (const [tabId, signal] of tabSignals.entries()) {
        if (!signal?.sampled_at_ms || now - signal.sampled_at_ms > CONTENT_TTL_MS) {
            tabSignals.delete(tabId);
        }
    }
}

async function buildReport() {
    const now = Date.now();
    trimExpiredSignals(now);

    const [tabs, windows, familyRaw, instanceId, idleState] = await Promise.all([
        api.tabs.query({}),
        api.windows.getAll({}),
        detectBrowserFamily(),
        getOrCreateInstanceId(),
        queryIdleState()
    ]);

    const focusedWindowIds = new Set(
        windows.filter((win) => win.focused).map((win) => win.id)
    );

    const tabPayloads = tabs.map((tab) => {
        const content = tabSignals.get(tab.id) ?? null;
        return {
            tab_id: tab.id,
            window_id: tab.windowId,
            active: Boolean(tab.active),
            audible: Boolean(tab.audible),
            hidden: Boolean(tab.hidden),
            discarded: Boolean(tab.discarded),
            last_accessed_ms: Number.isFinite(tab.lastAccessed) ? tab.lastAccessed : null,
            window_focused: focusedWindowIds.has(tab.windowId),
            url_origin: safeOrigin(tab.url ?? tab.pendingUrl),
            content
        };
    });

    return {
        version: 1, // legacy alias — removed in protocol v2
        protocol_version: DAEMON_PROTOCOL_VERSION,
        // Hint to the daemon: treat this report as authoritative for up to
        // 2× the periodic cadence. Daemon bounds this against its own
        // `signal_ttl_secs` config.
        veto_ttl_ms: Math.round(REPORT_PERIOD_MINUTES * 60_000 * 2),
        sent_at_ms: now,
        browser: {
            family: familyRaw,
            instance_id: instanceId,
            system_idle_state: idleState
        },
        summary: {
            tabs_total: tabPayloads.length,
            audible_tabs: tabPayloads.filter((tab) => tab.audible).length,
            hidden_tabs: tabPayloads.filter((tab) => tab.hidden).length,
            discarded_tabs: tabPayloads.filter((tab) => tab.discarded).length,
            active_tabs: tabPayloads.filter((tab) => tab.active).length,
            focused_windows: focusedWindowIds.size,
            content_samples: tabPayloads.filter((tab) => tab.content !== null).length
        },
        tabs: tabPayloads
    };
}

async function queryIdleState() {
    try {
        return await api.idle.queryState(60);
    } catch (_) {
        return "active";
    }
}

// ---------------------------------------------------------------------------
// Transport — Native Messaging Host (`bssl-ram-bridge`)
// ---------------------------------------------------------------------------
//
// The SW holds a single long-lived port into the NMH. When the port
// disconnects (SW hibernation, bridge exit, daemon unreachable at
// spawn time), the next send reconnects. In-flight requests are
// tracked in FIFO order — the NMH wire is one-in, one-out per port,
// so we can match replies by arrival order.

let _nativePort = null;
let _pendingResolvers = [];

function openNativePort() {
    try {
        const port = api.runtime.connectNative(NMH_HOST);
        port.onMessage.addListener((msg) => {
            const resolver = _pendingResolvers.shift();
            if (resolver) resolver.resolve(msg);
        });
        port.onDisconnect.addListener(() => {
            const err =
                api.runtime.lastError?.message ?? "bridge disconnected";
            _nativePort = null;
            for (const r of _pendingResolvers) r.reject(new Error(err));
            _pendingResolvers = [];
        });
        return port;
    } catch (err) {
        console.warn("bssl-ram signals: connectNative threw", err);
        return null;
    }
}

function getNativePort() {
    if (_nativePort) return _nativePort;
    _nativePort = openNativePort();
    return _nativePort;
}

function sendNative(msg) {
    return new Promise((resolve, reject) => {
        const port = getNativePort();
        if (!port) {
            reject(new Error("native messaging host unavailable"));
            return;
        }
        _pendingResolvers.push({resolve, reject});
        try {
            port.postMessage(msg);
        } catch (err) {
            _pendingResolvers = _pendingResolvers.filter((r) => r.resolve !== resolve);
            reject(err);
        }
    });
}

async function postToDaemon(report) {
    try {
        const resp = await sendNative({kind: "report", payload: report});
        if (!resp?.ok) {
            await noteFailure(
                `bssl-ram signals: bridge returned ${JSON.stringify(resp)}`
            );
            return false;
        }
        return true;
    } catch (err) {
        await noteFailure(`bssl-ram signals: ${err.message}`);
        return false;
    }
}

async function noteFailure(msg) {
    const now = Date.now();
    const stored = await api.storage.session.get(STORAGE_KEY_LAST_FAILURE_AT);
    const lastAt = Number.isFinite(stored?.[STORAGE_KEY_LAST_FAILURE_AT])
        ? stored[STORAGE_KEY_LAST_FAILURE_AT]
        : 0;
    if (now - lastAt > 60_000) {
        await api.storage.session.set({[STORAGE_KEY_LAST_FAILURE_AT]: now});
        console.warn(msg);
    }
}

async function pingDaemon() {
    try {
        const body = await sendNative({kind: "ping"});
        if (
            body &&
            Number(body.protocol_version) !== DAEMON_PROTOCOL_VERSION &&
            body.ok !== false
        ) {
            console.warn(
                `bssl-ram signals: daemon advertises protocol v${body.protocol_version}, ` +
                `extension expects v${DAEMON_PROTOCOL_VERSION}`
            );
        }
        return body;
    } catch (_) {
        return null;
    }
}

async function sendReport() {
    const report = await buildReport();
    await postToDaemon(report);
}

// ---------------------------------------------------------------------------
// Event wiring
// ---------------------------------------------------------------------------

api.runtime.onMessage.addListener((message, sender) => {
    if (!message || message.kind !== "bssl-content-signal" || !sender?.tab?.id) {
        return undefined;
    }

    tabSignals.set(sender.tab.id, normalizeContentSignal(message));
    void persistTabSignals();
    void scheduleReportSoon();
    return undefined;
});

api.tabs.onActivated.addListener(() => void scheduleReportSoon());
api.tabs.onUpdated.addListener(() => void scheduleReportSoon());
api.tabs.onRemoved.addListener((tabId) => {
    if (tabSignals.delete(tabId)) {
        void persistTabSignals();
    }
    void scheduleReportSoon();
});
api.windows.onFocusChanged.addListener(() => void scheduleReportSoon());
api.runtime.onStartup.addListener(() => void init());
api.runtime.onInstalled.addListener(() => void init());
api.idle.onStateChanged.addListener(() => void scheduleReportSoon());

api.alarms.onAlarm.addListener((alarm) => {
    if (alarm.name === REPORT_ALARM_TICK || alarm.name === REPORT_ALARM_DEBOUNCE) {
        void sendReport();
    }
});

// ---------------------------------------------------------------------------
// Rich-signals content script — opt-in via options page
// ---------------------------------------------------------------------------

async function hasRichPermission() {
    try {
        return await api.permissions.contains({origins: ["<all_urls>"]});
    } catch (_) {
        return false;
    }
}

async function syncRichScriptRegistration() {
    // `chrome.scripting.registerContentScripts` is a no-op unless we hold
    // the matching host permissions. We reconcile on startup and whenever
    // permissions change so the content script is registered exactly when
    // the user has granted broad host access.
    const granted = await hasRichPermission();

    try {
        const existing = await api.scripting.getRegisteredContentScripts({
            ids: [RICH_SCRIPT_ID]
        });
        if (existing.length && !granted) {
            await api.scripting.unregisterContentScripts({ids: [RICH_SCRIPT_ID]});
            return;
        }
        if (!existing.length && granted) {
            await api.scripting.registerContentScripts([
                {
                    id: RICH_SCRIPT_ID,
                    js: ["content-script.js"],
                    matches: ["<all_urls>"],
                    runAt: "document_idle",
                    persistAcrossSessions: true
                }
            ]);
        }
    } catch (err) {
        console.warn("bssl-ram signals: rich script reconciliation failed", err);
    }
}

if (api.permissions?.onAdded) {
    api.permissions.onAdded.addListener(() => void syncRichScriptRegistration());
}
if (api.permissions?.onRemoved) {
    api.permissions.onRemoved.addListener(() => void syncRichScriptRegistration());
}

// ---------------------------------------------------------------------------
// Init — runs on every SW wake (install, startup, and every cold re-exec)
// ---------------------------------------------------------------------------

async function init() {
    await hydrateTabSignals();
    await syncRichScriptRegistration();

    // Safety-net periodic tick. `alarms.create` is idempotent on name.
    try {
        await api.alarms.create(REPORT_ALARM_TICK, {
            periodInMinutes: REPORT_PERIOD_MINUTES
        });
    } catch (_) {
        /* alarms may be unavailable in exotic environments */
    }

    // Fire-and-forget handshake; we don't block report delivery on it.
    void pingDaemon();

    // First report — immediate so the daemon sees us quickly.
    void sendReport();
}

void init();
