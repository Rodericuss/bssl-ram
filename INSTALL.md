# Installing bssl-ram

This guide walks through a full install on a Linux box running a
systemd-managed user session. Three components need to end up on
disk:

1. **`bssl-ramd`** — the memory-compression daemon. System-level
   systemd unit at `bssl-ram@$USER.service`.
2. **`bssl-ram-bridge`** — the native-messaging host process the
   browser spawns. Per-user manifests under `~/.config/…/` and
   `~/.mozilla/…`.
3. **The browser extension** — Firefox 121+ or Chromium 121+
   Manifest V3 add-on that collects tab/window signals and hands
   them to the bridge.

Everything here assumes you've already satisfied the kernel
requirements listed in the top-level README (Linux ≥ 5.10, zram
configured as swap, `process_madvise(2)` available).

---

## 1. Build and install the daemon

Arch:

```bash
sudo pacman -S rust clang bpf libbpf bpftool zram-generator
```

Debian / Ubuntu 22.04+:

```bash
sudo apt install rustc cargo clang libbpf-dev bpftool zram-tools
```

Fedora:

```bash
sudo dnf install rust cargo clang libbpf-devel bpftool zram-generator
```

Then:

```bash
git clone https://github.com/Rodericuss/bssl-ram
cd bssl-ram
cargo build --release --workspace
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram
sudo install -Dm755 target/release/bssl-ram-bridge /usr/local/bin/bssl-ram-bridge
sudo install -Dm644 daemon/systemd/bssl-ram@.service /etc/systemd/system/bssl-ram@.service
```

Create a minimal `/etc/bssl-ram/config.toml` (every field is optional;
defaults are fine for most boxes):

```toml
# Enable the signals loop — opt-in, off by default.
signal_server_enabled = true

# Default transport is UDS at /run/bssl-ram/signals.sock. Leave as-is
# unless you have a specific reason to fall back to TCP.
# signal_transport = "tcp"
# signal_server_bind = "127.0.0.1:7879"
```

Start it:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now bssl-ram@$USER
journalctl -u bssl-ram@$USER -f
```

You should see a `browser signal server bound (uds)` line with the
socket path.

---

## 2. Install the native messaging host manifests

The bridge ships with its own installer — it knows the exact directory
each browser reads and writes the manifest file in place.

### Firefox (any 121+ build)

Firefox identifies the extension by the `bssl-ram-signals@bssl.io`
ID baked into the extension's manifest. Nothing else is needed:

```bash
bssl-ram-bridge install --user
```

### Chromium-family (Chrome, Chromium, Brave, Edge, Vivaldi, Opera)

Chrome requires the **extension ID** — the random-looking hex string
you see on `chrome://extensions` after loading the unpacked add-on.
Load the extension first (next section), copy the ID from
`chrome://extensions`, then re-run install with it:

```bash
# Load unpacked first, then:
bssl-ram-bridge install --user --chrome-ext-id aabbccddeeff00112233445566778899
```

Re-running `install` is idempotent — it overwrites existing manifest
files in-place. If you later move the bridge binary, re-run install
so the manifests point at the new path.

### System-wide install (optional)

Drop `--user` to write into `/etc/opt/chrome/native-messaging-hosts/`,
`/etc/chromium/native-messaging-hosts/`, and
`/usr/lib/mozilla/native-messaging-hosts/`. Usually needs sudo.

---

## 3. Load the extension

Firefox:

1. Open `about:debugging#/runtime/this-firefox`.
2. Click *Load Temporary Add-on…* and pick
   `extension/manifest.json`.
3. Open the extension's Options page to confirm the bridge is
   reachable (status should show *reachable* in green).

Chromium:

1. Open `chrome://extensions`, enable Developer Mode.
2. Click *Load unpacked* and pick the `extension/` directory.
3. Copy the extension ID shown on the card.
4. Run `bssl-ram-bridge install --user --chrome-ext-id <id>` (see
   step 2 above). Rerun the Options page refresh — it should now
   show *reachable*.

---

## 4. Verify

Options page rows should all be populated:

| Row                | Expected                                           |
|:-------------------|:---------------------------------------------------|
| Status             | reachable (green)                                  |
| Protocol           | v1                                                 |
| Accepted families  | firefox, chromium                                  |
| Max report         | ~1024 KiB                                          |
| Transport          | native-messaging-uds                               |
| Bridge             | `0.1.0` (or whatever you built)                    |
| Instance ID        | a UUID                                             |
| Last failure       | never                                              |

Daemon log should show `browser signal report accepted` lines a few
seconds after loading the extension. Compression decisions that hit
a veto log as `browser-side signal vetoed compression` with the
reason (`audible-tab`, `focused-window`, `playing-media`,
`recent-interaction`).

---

## 5. Uninstall

```bash
bssl-ram-bridge uninstall --user
sudo systemctl disable --now bssl-ram@$USER
sudo rm -f /usr/local/bin/bssl-ram /usr/local/bin/bssl-ram-bridge
sudo rm -f /etc/systemd/system/bssl-ram@.service
sudo rm -rf /etc/bssl-ram
```

Remove the extension from `about:debugging` or `chrome://extensions`
yourself — the installer does not touch loaded extensions.

---

## Troubleshooting

**Options page says "daemon unreachable"**
The bridge couldn't `connect()` the Unix socket. Check
`journalctl -u bssl-ram@$USER` for startup errors; the most common
cause is `signal_server_enabled` missing from `/etc/bssl-ram/config.toml`.

**Options page says "bridge timed out"**
The browser found the NMH manifest but couldn't execute the binary.
Double-check the manifest's `path` field points at a real executable
by running `bssl-ram-bridge --version` manually as your user.

**`bssl-ram-bridge install` wrote no files**
Without `--chrome-ext-id` the installer deliberately skips
Chromium-family dirs (Chrome rejects wildcard allowed_origins).
Pass the extension ID and re-run.

**Extension loads but no signals arrive**
Open the SW console (`chrome://extensions` → *Service worker* → *Inspect*,
or `about:debugging` → *Inspect* for Firefox) and look for warnings
from the transport layer. The bridge's own logs go to the browser's
NMH stderr capture, visible in the same console.
