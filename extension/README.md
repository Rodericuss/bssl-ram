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

Native Messaging Host + Unix domain socket. No HTTP, no network
sockets, no `host_permissions`.

```
extension ──(chrome.runtime.connectNative("io.bssl.ram"))──▶ bssl-ram-bridge
                                                                  │
                                                                  ▼
                                                        /run/bssl-ram/signals.sock
                                                                  │
                                                                  ▼
                                                            bssl-ramd (axum)
```

- The extension calls `chrome.runtime.connectNative("io.bssl.ram")`.
  The browser looks up `io.bssl.ram.json` under
  `~/.config/<browser>/NativeMessagingHosts/` (or
  `~/.mozilla/native-messaging-hosts/`), spawns the bridge binary
  with a stdio pipe, and passes the extension origin as `argv[1]`.
- The bridge forwards framed `{kind:"ping"}` and
  `{kind:"report",payload:...}` messages to the daemon over a Unix
  socket (`/run/bssl-ram/signals.sock` by default). Daemon UID
  check (`SO_PEERCRED`) is the real trust boundary.
- Replies come back through the same pipe to the extension.

See [`../INSTALL.md`](../INSTALL.md) for the install flow
(`bssl-ram-bridge install --user --chrome-ext-id <id>`).

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

- `tabs`, `windows`, `idle`, `alarms`, `storage`, `scripting`,
  `nativeMessaging`.

Optional (rich mode only):

- `<all_urls>` in `optional_host_permissions`. Granted/revoked through
  the options page via `chrome.permissions.request` / `.remove`.

Explicitly *not* asked for:

- Any `host_permissions` at all — the extension no longer speaks HTTP.
- `webRequest`, `webNavigation`, `downloads`, `history`, `bookmarks`,
  `cookies`, anything that touches user data.

## Local testing

See [`../INSTALL.md`](../INSTALL.md) for the complete install flow.
Short version:

```bash
# build daemon + bridge
cargo build --release --workspace
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram
sudo install -Dm755 target/release/bssl-ram-bridge /usr/local/bin/bssl-ram-bridge

# start daemon (signal_server_enabled = true in /etc/bssl-ram/config.toml)
sudo systemctl enable --now bssl-ram@$USER

# write NMH manifests
bssl-ram-bridge install --user --chrome-ext-id <id-after-loading-unpacked>
```

Then load the extension at `about:debugging` (Firefox) or
`chrome://extensions` (Chromium) and open the Options page — status
should show *reachable*.

If the daemon isn't running, report delivery fails silently — the
daemon will never force-compress on absence of data, so the feature
degrades safely to `/proc`-only heuristics.
