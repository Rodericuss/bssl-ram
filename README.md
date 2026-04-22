<div align="center">

<img src="https://capsule-render.vercel.app/api?type=waving&color=0:0d1117,50:1a1a2e,100:ff7139&height=200&section=header&text=bssl-ram&fontSize=72&fontColor=ff7139&animation=fadeIn&fontAlignY=38&desc=browsers%20should%20suckless%20ram&descSize=18&descAlignY=55&descColor=e5e7eb" width="100%" alt="header"/>

[![Rust](https://img.shields.io/badge/Rust-1.94+-ce422b?style=for-the-badge&logo=rust&logoColor=fff)](https://www.rust-lang.org/)
[![Linux](https://img.shields.io/badge/Linux_5.10+-000?style=for-the-badge&logo=linux&logoColor=ff7139)](https://kernel.org/)
[![Firefox](https://img.shields.io/badge/Firefox-any-ff7139?style=for-the-badge&logo=firefox&logoColor=fff)](https://www.mozilla.org/firefox/)
[![zram](https://img.shields.io/badge/zram-zstd-6e4a7e?style=for-the-badge)](https://wiki.archlinux.org/title/Zram)
[![License](https://img.shields.io/badge/license-MIT-0f3460?style=for-the-badge)](./LICENSE)

**588 MiB → 171 MiB. The pages were never touched, so nobody cares.**

---

*"Firefox asks for RAM. The kernel delivers. bssl-ram whispers to the kernel."*

</div>

---

> [!IMPORTANT]
> **bssl-ram is a tiny autonomous daemon that shrinks idle Firefox tabs by ~70% RSS without Firefox noticing.**
> It doesn't restart tabs, doesn't discard, doesn't reload. It just tells the kernel "page this out to zram — the user isn't looking". When the tab comes back, the kernel decompresses transparently on page fault. Firefox never learns this happened.

---

## ⚡ Quick Start

```bash
# 1. Build
cd daemon && cargo build --release

# 2. Make sure zram is on (Arch)
sudo pacman -S zram-generator

# 3. Install + enable the system template service for your user
sudo install -Dm755 target/release/bssl-ram /usr/local/bin/bssl-ram
sudo install -Dm644 systemd/bssl-ram@.service /etc/systemd/system/bssl-ram@.service
sudo systemctl daemon-reload
sudo systemctl enable --now bssl-ram@$USER.service

# 4. Watch it work
journalctl -u bssl-ram@$USER -f
```

The daemon runs as **your user** (not root) with `CAP_SYS_NICE` +
`CAP_SYS_PTRACE` ambient capabilities — enough to satisfy
`ptrace_may_access()` against your own Firefox without `sudo`.
Full setup notes: [`daemon/systemd/README.md`](daemon/systemd/README.md).

Prefer no systemd? Skip step 3 and use file capabilities:

```bash
sudo setcap cap_sys_nice,cap_sys_ptrace+eip /usr/local/bin/bssl-ram
/usr/local/bin/bssl-ram   # runs without sudo
```

Dry-run first if you're paranoid:

```toml
# /etc/bssl-ram/config.toml
dry_run = true
```

---

## 🏗️ Architecture

```mermaid
%%{init: {'theme': 'base', 'themeVariables': {
  'primaryColor': '#ff7139',
  'primaryTextColor': '#f8fafc',
  'primaryBorderColor': '#ce422b',
  'lineColor': '#ff7139',
  'secondaryColor': '#1a1a2e',
  'tertiaryColor': '#0d1117',
  'clusterBkg': '#0d1117',
  'clusterBorder': '#334155',
  'textColor': '#f8fafc',
  'fontFamily': 'ui-monospace, monospace'
}}}%%
flowchart LR
    subgraph Scan["🔎 every 10s"]
        PROC["/proc/*/cmdline<br/>find -isForBrowser ... tab"]
        STAT["/proc/PID/stat<br/>utime + stime delta"]
    end

    subgraph Decide["🧠 CpuTracker"]
        IDLE{"Δ ≤ 2 ticks<br/>for 3+ cycles?"}
    end

    subgraph Act["💨 compress_pid"]
        SMAPS["/proc/PID/smaps<br/>private anon regions"]
        PAGEOUT["process_madvise<br/>MADV_PAGEOUT via pidfd"]
    end

    subgraph Kernel["🧊 Linux kernel"]
        ZRAM["zram0 (zstd)<br/>~3:1 compression"]
        FAULT["on next page fault<br/>decompress transparent"]
    end

    PROC --> STAT --> IDLE
    IDLE -- yes --> SMAPS --> PAGEOUT --> ZRAM
    ZRAM -. user clicks tab .-> FAULT
```

The daemon is a single Tokio loop. Every `scan_interval_secs` it:

1. Walks `/proc` looking for processes whose cmdline contains `-isForBrowser` and ends in `tab` — these are Firefox content processes hosting browser tabs (not rdd, utility, socket, gpu, or forkserver).
2. Reads `utime + stime` from `/proc/PID/stat` and diffs against the previous snapshot. Tabs that burn ≤ 2 ticks (20ms CPU) per cycle for 3 consecutive cycles are flagged idle.
3. Parses `/proc/PID/smaps`, selects only **private anonymous** regions (perms `p`, inode 0, `Anonymous: > 0 kB`), and calls `process_madvise(pidfd, iov, MADV_PAGEOUT)` on each.

That's the whole thing. No ptrace, no signals, no process suspension. The kernel handles decompression on demand — Firefox doesn't know its pages moved.

---

## 📊 What actually happens

Measured on a real Firefox tab with 588 MiB RSS, using `examples/compress_real.rs`:

| Metric | Before | After | Δ |
|:-------|-------:|------:|-----:|
| **RSS** | 588 MiB | 171 MiB | **−417 MiB (−70%)** |
| **PSS** | 493 MiB | 65 MiB | −428 MiB |
| **Swap (zram)** | 3 MiB | 374 MiB | **+374 MiB** |
| **Syscall time** | — | — | 1.38s for 1000 regions |

Net physical RAM returned to the system after zstd compression: about **260 MiB** from a single tab. Firefox continued running. The tab, when switched back to, was indistinguishable from a non-compressed one.

---

## ⚙️ Configuration

`/etc/bssl-ram/config.toml` — all fields optional, defaults shown:

```toml
scan_interval_secs    = 10   # seconds between /proc scans
idle_cycles_threshold = 3    # consecutive idle cycles before compressing (3 × 10s = 30s)
cpu_delta_threshold   = 2    # CPU ticks per cycle to be considered idle (2 ticks = 20ms)
min_rss_mib           = 50   # don't bother compressing tiny processes
dry_run               = false
```

---

## 🧪 Development

The daemon ships with four inspection examples that bypass the main loop and let you validate each subsystem in isolation:

```bash
# list PIDs the scanner finds — diff against `ps aux | grep isForBrowser`
cargo run --example scan_test

# watch CPU ticks per tab live (env: CYCLES, INTERVAL)
cargo run --example cpu_test

# inspect smaps parsing without compressing anything
cargo run --example compress_test

# real compression with before/after RSS (needs sudo)
sudo ./target/debug/examples/compress_real
```

---

## 📦 Requirements

| Requirement | Why |
|:---|:---|
| Linux kernel ≥ 5.10 | `process_madvise` and `pidfd_open` syscalls |
| zram configured as swap | Without it, pages go to disk — defeats the point |
| `CAP_SYS_NICE` + `CAP_SYS_PTRACE` | Granted by the systemd unit or via `setcap` — no permanent root |
| Firefox | Any recent version |

---

## 🧯 Known limitations

- **No per-tab granularity.** Firefox's Fission keeps tabs from the same site in the same content process — compressing one compresses all siblings. Acceptable since they'll all idle together.
- **Background media detection.** A tab playing audio through MSE may show low CPU delta because the actual decoding happens in the `rdd` process. Future work: a D-Bus MPRIS listener to globally block compression during `PlaybackStatus=Playing`.
- **WebRTC / Meet / Zoom.** These rarely expose `MediaSession`, so MPRIS won't help. Future work: a minimal Native Messaging Host as a cooperative "please don't compress" signal from the page.
- **Cold-start latency.** The first access after compression pays a page-fault roundtrip plus zstd decompression (sub-100ms for typical tab working sets — noticeable but not painful).

---


<div align="center">

**`bssl` — browsers should suckless.**

</div>
