<div align="center">

<img src="https://capsule-render.vercel.app/api?type=waving&color=0:0d1117,50:1a1a2e,100:ff7139&height=160&section=header&text=systemd&fontSize=60&fontColor=ff7139&animation=fadeIn&fontAlignY=40&desc=running%20bssl-ram%20as%20a%20proper%20service&descSize=16&descAlignY=62&descColor=e5e7eb" width="100%" alt="header"/>

[![systemd](https://img.shields.io/badge/systemd-%E2%89%A5%20251-000?style=for-the-badge&logo=linux&logoColor=ff7139)](https://systemd.io/)
[![Linux](https://img.shields.io/badge/Linux_5.10+-000?style=for-the-badge&logo=linux&logoColor=ff7139)](https://kernel.org/)
[![Capabilities](https://img.shields.io/badge/CAP_SYS__PTRACE-%2BCAP__SYS__NICE-ce422b?style=for-the-badge)](https://man7.org/linux/man-pages/man7/capabilities.7.html)
[![License](https://img.shields.io/badge/license-MIT-0f3460?style=for-the-badge)](../../LICENSE)

**Ambient caps, no permanent root. No `sudo` once installed.**

---

*"`--user` doesn't work. System template with `User=%i` does."*

</div>

---

> [!IMPORTANT]
> Run `bssl-ram` as a **system template service** that drops to your
> user UID and keeps `CAP_SYS_NICE` + `CAP_SYS_PTRACE` as ambient
> capabilities. The unit ships with a moderate sandbox
> (`ProtectSystem=strict`, `ProtectHome=read-only`,
> `RestrictAddressFamilies=AF_UNIX AF_NETLINK`, etc.) and provisions
> `/run/bssl-ram/` at mode `0700` for the optional signals UDS.

---

## 🧠 Why a system template (and not `--user`)

`process_madvise(2)` and reading `/proc/PID/smaps_rollup` both go through
`ptrace_may_access()`. On Arch with the default
`kernel.yama.ptrace_scope = 1` that check succeeds only if the caller holds
`CAP_SYS_PTRACE` **in the same user namespace as the target**.

`systemd --user` services run inside their own user namespace, so any
ambient cap granted there cannot satisfy a ptrace check against Firefox
running in the init userns. A system service can drop privileges with
`User=` while staying in the init userns, which keeps the caps usable.

The unit is a template (`bssl-ram@.service`), instantiated per user:

```bash
sudo systemctl start bssl-ram@$USER.service
```

---

## ⚡ Install

```bash
# 1. Build + install the binary
cd daemon
cargo build --release
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram

# 2. Install the template unit
sudo install -Dm644 systemd/bssl-ram@.service \
    /etc/systemd/system/bssl-ram@.service
sudo systemctl daemon-reload

# 3. Enable for your user
sudo systemctl enable --now bssl-ram@$USER.service
```

Full end-to-end install (daemon + bridge + extension) lives in
[`../../INSTALL.md`](../../INSTALL.md).

---

## ✅ Verify

```bash
systemctl status bssl-ram@$USER.service
journalctl -u bssl-ram@$USER -f

# Inspect runtime caps
PID=$(systemctl show bssl-ram@$USER.service -p MainPID --value)
capsh --decode=$(awk '/^CapEff:/ {print $2}' /proc/$PID/status)
# expect: cap_sys_ptrace,cap_sys_nice   (+ cap_bpf,cap_perfmon if enabled)
```

After ~30 s of any tab idling you should see compression in the journal:

```
INFO cycle{n=3 targets=12 breakdown=chromium=8 firefox=4}: page-out done
     pid=222139 profile=firefox action=compress reason=idle
     rss_mib=85 regions=63 bytes_advised_mib=50 batches=1 dry_run=false
```

Each per-PID decision is one structured line with `action=` and
`reason=` fields — grep `action=compress` for what the daemon **did**,
or `action=skip` for what it deliberately ignored.

---

## 🛡️ Capabilities granted

| Capability         | Why                                                                                              |
|:-------------------|:-------------------------------------------------------------------------------------------------|
| `CAP_SYS_NICE`     | Pass the capability gate `process_madvise(2)` got in kernel 5.12.                                |
| `CAP_SYS_PTRACE`   | Bypass Yama ptrace_scope = 1 when reading `/proc/PID/smaps_rollup` and opening a target `pidfd`. |
| `CAP_SYS_RESOURCE` | Register a PSI memory-pressure trigger against `/proc/pressure/memory` with `O_RDWR`. Optional.  |
| `CAP_NET_ADMIN`    | Bind the `cn_proc` netlink connector on older kernels. Optional on ≥ 6.1.                        |
| `CAP_BPF`          | Load the `cpu_tracker` eBPF skeleton when `enable_bpf_cpu_tracker = true`. Optional.             |
| `CAP_PERFMON`      | Attach the `sched_switch` tracing program. Paired with `CAP_BPF`.                                |

Every optional capability falls back to a timer / `/proc` path when the
kernel or your config declines to grant it. The daemon logs a warning
and keeps running.

---

## 🧾 Logging & telemetry

Two env vars control output (`Environment=` in a drop-in, or inline
when running standalone):

| Variable          | Default  | Meaning                                                                                                                |
|:------------------|:---------|:-----------------------------------------------------------------------------------------------------------------------|
| `RUST_LOG`        | `info`   | Standard tracing EnvFilter. e.g. `bssl_ram=debug` for per-skip decisions, `bssl_ram::scanner=trace` for low-level.     |
| `BSSL_LOG_FORMAT` | `pretty` | `pretty` (human, ANSI), `compact` (one-liner), `json` (one JSON object per event — pipe to `jq`, Loki, Elasticsearch). |

### Useful one-liners

```bash
# Only what got compressed
journalctl -u bssl-ram@$USER -f | grep 'action=compress'

# Per-cycle scoreboard (default every ~10 min)
journalctl -u bssl-ram@$USER -f | grep 'stats snapshot'

# Timeline for one PID
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
     bytes_skipped_mib=12 skip_warmup=8 skip_active=180
     skip_already_compressed=1620 skip_low_rss=10 skip_browser_signals=18
     errors=0 psi_events=4
```

---

## 🧊 Sandbox posture

The shipped unit applies:

| Directive                 | Value                                       |
|:--------------------------|:--------------------------------------------|
| `ProtectSystem`           | `strict`                                    |
| `ProtectHome`             | `read-only`                                 |
| `PrivateTmp`              | `true`                                      |
| `PrivateDevices`          | `true`                                      |
| `RestrictAddressFamilies` | `AF_UNIX AF_NETLINK` (denies AF_INET/INET6) |
| `ProtectClock`            | `true`                                      |
| `ProtectControlGroups`    | `true`                                      |
| `ProtectHostname`         | `true`                                      |
| `ProtectKernelLogs`       | `true`                                      |
| `ProtectKernelModules`    | `true`                                      |
| `LockPersonality`         | `true`                                      |
| `RuntimeDirectory`        | `bssl-ram` (mode `0700`)                    |
| `MemoryMax` / `TasksMax`  | `128M` / `16`                               |

> [!NOTE]
> The heavier options (`PrivateUsers`, `ProtectKernelTunables`,
> `RestrictNamespaces`, `SystemCallFilter`) are **deliberately off** —
> they interfere with the `ptrace_may_access()` checks the daemon
> needs to make.

Audit the resulting posture with:

```bash
systemd-analyze security bssl-ram@$USER
```

---

## 🧪 Alternative: file capabilities (no systemd)

If you prefer to skip systemd entirely:

```bash
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram
sudo setcap cap_sys_nice,cap_sys_ptrace+eip /usr/local/bin/bssl-ram
/usr/local/bin/bssl-ram
```

The capabilities are baked into the binary's xattrs, so it doesn't
need `sudo` at runtime. You give up auto-restart, journald integration
and the sandbox posture, but it works the same.

---

## 🧭 See also

- [`../../README.md`](../../README.md) — project overview.
- [`../../INSTALL.md`](../../INSTALL.md) — end-to-end install.
- [`../../extension/README.md`](../../extension/README.md) — browser-
  side signal extension.
- [`../../bench/README.md`](../../bench/README.md) — reproducible
  benchmark suite.
- `bssl-ram@.service` in this directory — the actual unit file.

---

<div align="center">

**`bssl` — running as the right user, holding the right caps, doing the right work.**

</div>
