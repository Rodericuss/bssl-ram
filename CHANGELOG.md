# Changelog

All notable changes to **bssl-ram** are recorded here. The format
follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/);
versioning is [SemVer 2.0.0](https://semver.org/spec/v2.0.0.html).

The workspace now ships two binaries (`bssl-ram` daemon,
`bssl-ram-bridge` native-messaging host) plus a browser extension under
`extension/`. Entries below mark which component a change affects.

---

## [Unreleased]

## [0.4.0] — 2026-04-22 — *Signals tier-S*

Second-generation browser-signals integration. The loopback-HTTP
scaffold shipped experimentally under `0.3.x` is replaced by a
Native-Messaging-Host bridge talking to the daemon over a Unix
socket with `SO_PEERCRED` UID assertion — the same pattern KeePassXC
and 1Password use for their browser-side helpers.

### Added

- **daemon** — UDS transport for the signals server
  (`signal_transport = "uds"` default, `signal_uds_path =
  "/run/bssl-ram/signals.sock"`). Explicit `chmod 0600` after bind;
  per-connection `SO_PEERCRED` UID assertion before the router sees
  the request. TCP transport kept behind `signal_transport = "tcp"`
  for local iteration.
- **daemon** — `GET /v1/signals/ping` handshake endpoint advertising
  `protocol_version`, `accepted_families`, `max_report_bytes`. Lets
  the extension detect wire-version mismatch before emitting reports.
- **daemon** — schema fields `protocol_version`, `browser.instance_id`
  (UUID v4), `veto_ttl_ms` on the report payload. `version` kept as a
  legacy alias for one release.
- **daemon** — `sent_at_ms` anti-skew rejection (±120s from wall
  clock) — defangs a hibernated SW flushing a stale batch.
- **daemon** — 1 MiB body limit on `/v1/signals/report` (was 256 KiB),
  enough headroom for a 1000-tab session.
- **daemon** — `SocketCleanup` RAII guard that unlinks the UDS file
  when the last `Arc<SignalStore>` dies. `RuntimeDirectory=bssl-ram`
  covers the happy systemd path; this catches `cargo run` / SIGKILL.
- **daemon** — new `skips_browser_signals` telemetry counter.
- **bridge** — new `bssl-ram-bridge` crate. NMH frame codec (4-byte
  native-endian u32 + UTF-8 JSON), bare HTTP/1.1 client over
  `UnixStream` via `hyper::client::conn::http1::handshake`, CLI
  `install` / `uninstall` subcommands that write per-user NMH
  manifests for Chrome, Chromium, Brave, Edge, Opera, Vivaldi,
  Firefox, LibreWolf, Waterfox.
- **extension** — MV3-SW-hibernation-safe background. `chrome.alarms`
  replaces every `setTimeout`; `tabSignals` persisted as an array in
  `chrome.storage.session`; instance UUID in `chrome.storage.local`.
- **extension** — options page (`options.html` + `options.js`) with
  daemon health check, protocol version display, instance ID, last
  failure timestamp, and a rich-signals opt-in toggle driven by
  `chrome.permissions.request` + `chrome.scripting.registerContentScripts`.
- **extension** — `chrome.runtime.connectNative("io.bssl.ram")`
  transport replacing loopback `fetch`. Long-lived port with FIFO
  reply-matching; auto-reconnects on SW hibernation.
- **repo** — `CHANGELOG.md` (this file), `INSTALL.md` end-to-end
  install guide, `LICENSE` (MIT), `CODE_OF_CONDUCT.md` (Contributor
  Covenant 2.1), `CONTRIBUTING.md`, `SECURITY.md` with threat model,
  `.github/PULL_REQUEST_TEMPLATE.md`, four GitHub issue form templates
  (bug, feature, perf regression, profile request).

### Changed

- **daemon** — `daemon/Cargo.toml` version `0.3.0` → `0.4.0`. Repo is
  now a Cargo workspace (`/Cargo.toml` at the root) with shared
  `[workspace.dependencies]` for tokio, hyper, hyper-util,
  http-body-util, serde, serde_json, anyhow, tracing. `[profile.release]`
  moved to the workspace root.
- **daemon/systemd** — `PrivateNetwork=true` removed (it silently
  blocked loopback TCP and would have blocked the new UDS accept
  loop too). Replaced with `RestrictAddressFamilies=AF_UNIX AF_NETLINK`
  which denies AF_INET/AF_INET6 egress while allowing the signals UDS
  and the existing cn_proc netlink subscription. Added
  `RuntimeDirectory=bssl-ram` (mode 0700) so `/run/bssl-ram/` exists
  for the socket at boot and disappears on Stop.
- **daemon/signals** — `SignalStore` no longer implements `Default`.
  A zero `Duration` TTL silently made every report look stale; the
  new constructor forces `new(ttl, interaction_grace)`.
- **daemon/signals** — `apply_report` split into
  `apply_report(report)` and `apply_report_at(now_wall, report)` so
  tests can inject a synthetic wall clock.
- **extension/manifest** — version `0.1.0` → `0.3.0`,
  `minimum_chrome_version = "121"`, `strict_min_version = "121.0"`,
  `nativeMessaging` permission added, `host_permissions` removed
  entirely, `<all_urls>` moved to `optional_host_permissions`.
- **extension/content scripts** — no longer injected by a static
  `content_scripts` manifest block. Registered programmatically via
  `chrome.scripting.registerContentScripts` only after the user
  grants `<all_urls>` in the options page.

### Removed

- **extension** — loopback HTTP transport. The three candidate
  endpoints (`127.0.0.1:7879`, `localhost:7879`, `[::1]:7879`), the
  `STORAGE_KEY_ENDPOINT_INDEX` round-robin cache, and the
  `x-bssl-signal-source` header on the extension side. The daemon
  still honors `x-bssl-signal-source` on TCP transport for dev.
- **extension** — `host_permissions` block (was the three loopback
  daemon URLs).

### Security

- Same-UID signal forgery via `curl http://127.0.0.1:7879/...` is no
  longer possible by default. The new trust boundary is the NMH
  stdio pipe (created by the browser's own `fork/exec`) plus the
  UDS with chmod 0600 and `SO_PEERCRED` UID assertion. The
  `x-bssl-signal-source` header was never authentication and is
  documented as such — it remains on TCP as defense-in-depth only.
- `SignalStore::default()` footgun removed — would have silently
  neutered the feature with a zero-duration TTL if any future code
  path called it.
- `PrivateNetwork=true` removal is documented as a net tightening:
  paired with `RestrictAddressFamilies=AF_UNIX AF_NETLINK` the daemon
  still has no AF_INET/AF_INET6 reachability.

### Internal

- Test count: daemon 70 → 70 (3 retained, replaced 3 with
  clock-injection variants), bridge +6 (frame codec 3, manifest 2,
  target-dir discovery 1). Workspace total **76 passing**.
- `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --all -- --check` clean across both crates.

### Upgrade notes

- The systemd unit changed. After pulling:

  ```bash
  sudo install -Dm644 daemon/systemd/bssl-ram@.service \
      /etc/systemd/system/bssl-ram@.service
  sudo systemctl daemon-reload
  sudo systemctl restart bssl-ram@$USER
  ```

- To enable signals end-to-end, follow `INSTALL.md`. Existing
  `/etc/bssl-ram/config.toml` stays compatible — `signal_server_enabled`
  still defaults to `false`, and if you had it `true` the new defaults
  (UDS transport at `/run/bssl-ram/signals.sock`) apply automatically.

- Operators who prefer the old loopback-TCP behavior (development,
  test harnesses) can set:

  ```toml
  signal_transport = "tcp"
  signal_server_bind = "127.0.0.1:7879"
  ```

  — the server still responds on that path, but all same-UID spoofing
  risk from `0.3.x` applies.

---

## [0.3.0] — 2026-04-22 — *BPF authoritative*

### Added

- **daemon/bpf** — eBPF `sched_switch` CPU tracker replaces
  `/proc/PID/stat` polling in the hot path. Kernel maps are
  authoritative when `enable_bpf_cpu_tracker = true`; /proc is only
  read as a cold-start fallback. (`8e533a3`, `ef25c5f`)
- **bench** — reproducible benchmark suite under `bench/`: harness
  scripts, result tables, R analyzer, GitHub-native plots.
  (`bb710a1`, `a2baa8b`, `52942da`)

### Changed

- **ci** — `build.rs` picked up a rustfmt violation and was fixed
  under the existing format-on-commit workflow. (`065bcd2`)

---

## [0.2.0] — prior

### Added

- **daemon/psi** — event-driven PSI memory-pressure trigger; idles
  between timer ticks when the system is comfortable, wakes
  immediately when `/proc/pressure/memory` crosses the configured
  stall threshold.
- **daemon/proc_connector** — `cn_proc` netlink subscription for
  fork/exec/exit events; the in-memory process table replaces the
  per-cycle `/proc` walk (safety-net drift-correction reseed every N
  cycles).
- **daemon/scanner** — profile-driven matching for any
  Firefox-family, Chromium-family, or Electron app.
- **daemon/state** — dual-threshold CPU logic kills the recompression
  cascade triggered by browser GC bursts. Starttime + GC ensures
  PID-reuse doesn't confuse the tracker.
- **daemon/telemetry** — structured per-PID decision logs, runtime
  stats snapshots, JSON logging.
- **ci** — GitHub Actions workflow, release tarball packaging on tag.

---

## [0.1.0] — initial

- First working daemon: scans `/proc`, diffs CPU ticks, detects idle
  renderer processes, calls `process_madvise(MADV_PAGEOUT)` in
  `IOV_MAX` chunks. Runs with ambient `CAP_SYS_NICE` +
  `CAP_SYS_PTRACE` under the shipped systemd template unit.
- First supported profiles: Firefox tabs (`-isForBrowser ... tab`)
  and Chromium renderers (`--type=renderer`, with extension /
  gpu / utility / zygote / crashpad exclusions).

---

[Unreleased]: https://github.com/Rodericuss/bssl-ram/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/Rodericuss/bssl-ram/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Rodericuss/bssl-ram/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Rodericuss/bssl-ram/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Rodericuss/bssl-ram/releases/tag/v0.1.0
