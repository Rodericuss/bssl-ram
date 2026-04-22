# bssl-ram signal extension

Companion extension for the `bssl-ram` daemon. The daemon decides by
itself whether a renderer is compressible; this extension sends extra
context that `/proc` cannot infer, so the daemon can *veto* compression
when the page is still meaningful to the user.

It does not track browsing, does not send page content, and does not
store anything identifying beyond a per-install UUID used to tell two
browser profiles apart.

## What it reports

Two tiers, by user consent:

**Coarse signals (always on, zero extra permissions beyond the local
daemon URL):**

- Which browser window is focused.
- Which tabs are active, audible, hidden, discarded, or recently accessed.
- System idle state (active / idle / locked), from `chrome.idle`.

Those alone give the daemon a solid veto for the "user is playing
something" and "user has this browser in the foreground" cases.

**Rich signals (opt-in from the options page — requires `<all_urls>`):**

- Per-page `visibilityState`, `document.hasFocus()`, fullscreen,
  Picture-in-Picture.
- Count of `<audio>` / `<video>` elements, and how many are *actually
  playing* right now (catches MSE / WebRTC flows that `tab.audible`
  misses).
- Last user interaction timestamp (pointer / keyboard / wheel / touch).

The content script is registered *programmatically* via
`chrome.scripting.registerContentScripts` only after the user has
granted `<all_urls>` in the options page. Revoking the permission
unregisters it.

## Transport (protocol v1)

Loopback HTTP, JSON, single endpoint per browser session.

- `GET  http://127.0.0.1:7879/v1/signals/ping` → handshake.
  Extension calls this on startup to verify `protocol_version` matches.
- `POST http://127.0.0.1:7879/v1/signals/report` → report body (see
  below). Requires header `x-bssl-signal-source: extension`.
- Two additional fallback endpoints are tried in order on error:
  `http://localhost:7879/...`, `http://[::1]:7879/...`.

All endpoints are loopback; the daemon rejects anything else. The
`x-bssl-signal-source` header is a marker, not an authenticator —
real cryptographic auth lands with the native-messaging + Unix-socket
transition. See the project `SECURITY.md`.

## Report shape (v1)

```jsonc
{
  "version": 1,                       // legacy alias, removed in v2
  "protocol_version": 1,
  "veto_ttl_ms": 60000,               // hint: how long this veto should hold
  "sent_at_ms": 1713720000000,
  "browser": {
    "family": "firefox",              // canonicalized by daemon: firefox|chromium
    "instance_id": "550e8400-…",      // UUID v4 persisted in storage.local
    "system_idle_state": "active"
  },
  "summary": {
    "tabs_total": 12,
    "audible_tabs": 1,
    "hidden_tabs": 3,
    "discarded_tabs": 0,
    "active_tabs": 2,
    "focused_windows": 1,
    "content_samples": 5
  },
  "tabs": [
    {
      "tab_id": 123,
      "window_id": 4,
      "active": false,
      "audible": false,
      "hidden": true,
      "discarded": false,
      "last_accessed_ms": 1713719995000,
      "window_focused": false,
      "url_origin": "https://example.com",   // origin-only, never path/query
      "content": {
        "visibility_state": "hidden",
        "document_has_focus": false,
        "fullscreen": false,
        "picture_in_picture": false,
        "media_elements": 1,
        "playing_media_elements": 0,
        "last_user_interaction_ms": 1713719900000,
        "sampled_at_ms": 1713720000000
      }
    }
  ]
}
```

Every field is optional on the wire — the daemon uses `#[serde(default)]`
— so older extensions and the v1 daemon continue to interoperate.

## Veto semantics (daemon side)

The daemon treats a fresh report as authoritative for the whole
matching browser *family* until `signal_ttl_secs` expires. Veto
priority, first match wins:

1. **audible-tab** — any tab with `audible: true`.
2. **playing-media** — sum of `playing_media_elements` > 0 across rich
   signals.
3. **focused-window** — any tab with `window_focused: true`.
4. **recent-interaction** — any rich signal whose
   `last_user_interaction_ms` is within `signal_interaction_grace_secs`.

Because the daemon does not (yet) have a stable tab→PID mapping on
Firefox or Chromium stable, a veto currently applies to *all*
renderers of the matching family. This is intentionally coarse. A
more granular model is tracked under the native-messaging migration.

## Service worker lifecycle — what we actually design for

`chrome.alarms`, not `setTimeout`. `storage.session`, not globals.
Specifically:

- The SW may be killed after ~30s of idle. Every `let` / `const` at
  the top of `background.js` is re-initialized on wake. `tabSignals`
  rehydrates from `chrome.storage.session` inside `init()`.
- Report scheduling uses two `chrome.alarms`: a periodic safety-net
  (`bssl-ram-report-tick`, 30s) and a one-shot debounce
  (`bssl-ram-report-debounce`) that gets recreated on each event. In
  production Chrome the debounce floors at 30s; in unpacked dev mode
  it floors at 1s.
- Browser family, idle state, and instance ID are fetched inside
  `buildReport()` so a report built after a cold SW start is still
  complete.

## Options page

`chrome://extensions → bssl-ram signals → Extension options`:

- Daemon reachability (hits `/v1/signals/ping`, shows protocol version
  and the accepted families list returned by the daemon).
- Extension version, browser family, instance ID, last transport
  failure timestamp.
- Rich-signals toggle (asks for `<all_urls>`, registers/unregisters
  the content script accordingly).

## Permissions footprint

Required (baseline):

- `tabs`, `windows`, `idle`, `alarms`, `storage`, `scripting`.
- `host_permissions` for the three loopback daemon URLs.

Optional (rich mode only):

- `<all_urls>` in `optional_host_permissions`. Granted/revoked through
  the options page via `chrome.permissions.request` / `.remove`.

Explicitly *not* asked for:

- `nativeMessaging` (coming with the NMH+UDS transition, still v1 HTTP
  here).
- `webRequest`, `webNavigation`, `downloads`, `history`, `bookmarks`,
  `cookies`, anything that touches user data.

## Local testing

Firefox 121+:

1. Open `about:debugging#/runtime/this-firefox`.
2. Load Temporary Add-on → pick `manifest.json`.

Chromium 121+:

1. Open `chrome://extensions`, enable Developer Mode.
2. Load unpacked → pick the `extension/` directory.

Run the daemon with the signals server on:

```toml
# /etc/bssl-ram/config.toml
signal_server_enabled = true
signal_server_bind = "127.0.0.1:7879"
```

Then watch it:

```bash
journalctl -u bssl-ram@$USER -f | grep -E "signal|browser"
```

The extension's options page shows whether the daemon is reachable.
If the daemon isn't running, report delivery fails silently — the
daemon will never force-compress on absence of data, so the feature
degrades safely to `/proc`-only heuristics.
