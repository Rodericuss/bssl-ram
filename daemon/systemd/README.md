# systemd integration

Run `bssl-ram` as a **system template service** that drops to your user UID
and keeps `CAP_SYS_NICE` + `CAP_SYS_PTRACE` as ambient capabilities — no
permanent root, no `sudo` after install.

## Why a system template service (and not `--user`)

`process_madvise(2)` and reading `/proc/PID/smaps_rollup` both go through
`ptrace_may_access()`. On Arch with the default `kernel.yama.ptrace_scope=1`
those checks succeed only if the caller has `CAP_SYS_PTRACE` **in the same
user namespace as the target**.

`systemd --user` services run inside their own user namespace, so any
ambient cap granted there cannot satisfy a ptrace check against Firefox
running in the init userns. A system service can drop privileges with
`User=` while staying in the init userns, which keeps the caps usable.

The unit is a template (`bssl-ram@.service`), instantiated per user:

```
sudo systemctl start bssl-ram@username.service
```

## Install

```bash
# 1. Install the binary
cd daemon
cargo build --release
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram

# 2. Install the template unit
sudo install -Dm644 systemd/bssl-ram@.service /etc/systemd/system/bssl-ram@.service
sudo systemctl daemon-reload

# 3. Enable for your user (replace with your actual login)
sudo systemctl enable --now bssl-ram@$USER.service
```

## Verify

```bash
systemctl status bssl-ram@$USER.service
journalctl -u bssl-ram@$USER -f

# Check the runtime caps
PID=$(systemctl show bssl-ram@$USER.service -p MainPID --value)
capsh --decode=$(awk '/^CapEff:/ {print $2}' /proc/$PID/status)
# expect: cap_sys_ptrace,cap_sys_nice
```

After ~30s of any tab being idle you should see compression in the journal:

```
INFO cycle{n=3 targets=12 breakdown=chromium=8 firefox=4}: page-out done
     pid=222139 profile=firefox action=compress reason=idle
     rss_mib=85 regions=63 bytes_advised_mib=50 batches=1 dry_run=false
```

Each per-PID decision is one structured line with `action=` and `reason=`
fields — grep `action=compress` to see only what the daemon **did**, or
`action=skip` to see what it deliberately ignored.

## Logging & telemetry

Two env vars control output (set via `Environment=` in a drop-in or pass
inline when running directly):

| Variable | Default | Meaning |
|:---|:---|:---|
| `RUST_LOG` | `info` | Standard tracing EnvFilter. e.g. `bssl_ram=debug` to see per-skip decisions, `bssl_ram::scanner=trace` for low-level. |
| `BSSL_LOG_FORMAT` | `pretty` | `pretty` (human, ANSI), `compact` (one-liner), or `json` (one JSON object per event — pipe to `jq` or send to Loki/Elasticsearch). |

Useful one-liners:

```bash
# Watch only what got compressed
journalctl -u bssl-ram@$USER -f | grep 'action=compress'

# Watch only the per-cycle scoreboard (default every 60 cycles ≈ 10 min)
journalctl -u bssl-ram@$USER -f | grep 'stats snapshot'

# Per-PID timeline (great when "did pid 12345 ever get compressed?")
journalctl -u bssl-ram@$USER --since "1 hour ago" | grep 'pid=12345'

# JSON ingest into jq for ad-hoc analysis
BSSL_LOG_FORMAT=json /usr/local/bin/bssl-ram \
  | jq -c 'select(.fields.action == "compress")
           | {pid: .fields.pid, profile: .fields.profile,
              mib: .fields.bytes_advised_mib}'
```

Tune the snapshot cadence in `/etc/bssl-ram/config.toml`:

```toml
telemetry_interval_cycles = 60   # snapshot every N cycles, 0 disables
```

A snapshot looks like:

```
INFO bssl-ram stats snapshot
     scans=120 targets_seen=1860 compressions=42 bytes_paged_out_mib=3870
     bytes_skipped_mib=12 skip_warmup=8 skip_active=180 skip_already_compressed=1620
     skip_low_rss=10 errors=0
```

## Sandbox notes

The unit applies a moderate sandbox (`ProtectSystem=strict`,
`ProtectHome=read-only`, `PrivateNetwork`, `MemoryMax=128M`, etc.). It
deliberately does **not** enable the heavy options (`PrivateUsers`,
`ProtectKernelTunables`, `RestrictNamespaces`, `SystemCallFilter`) because
those interfere with the very `ptrace_may_access()` checks the daemon
needs to make.

Audit the resulting posture with:

```bash
systemd-analyze security bssl-ram@$USER
```

## Alternative: file capabilities (no systemd)

If you prefer to skip systemd entirely:

```bash
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram
sudo setcap cap_sys_nice,cap_sys_ptrace+eip /usr/local/bin/bssl-ram
/usr/local/bin/bssl-ram
```

The capabilities are baked into the binary's xattrs, so it doesn't need
`sudo` at runtime. You give up auto-restart and journald integration, but
it works the same.
