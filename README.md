<div align="center">

<img src="https://capsule-render.vercel.app/api?type=waving&color=0:0d1117,50:1a1a2e,100:ff7139&height=200&section=header&text=bssl-ram&fontSize=72&fontColor=ff7139&animation=fadeIn&fontAlignY=38&desc=browsers%20should%20suckless%20ram&descSize=18&descAlignY=55&descColor=e5e7eb" width="100%" alt="header"/>

[![Rust](https://img.shields.io/badge/Rust-1.94+-ce422b?style=for-the-badge&logo=rust&logoColor=fff)](https://www.rust-lang.org/)
[![Linux](https://img.shields.io/badge/Linux_5.10+-000?style=for-the-badge&logo=linux&logoColor=ff7139)](https://kernel.org/)
[![Firefox](https://img.shields.io/badge/Firefox-family-ff7139?style=for-the-badge&logo=firefox&logoColor=fff)](https://www.mozilla.org/firefox/)
[![Chromium](https://img.shields.io/badge/Chromium-family-4285f4?style=for-the-badge&logo=googlechrome&logoColor=fff)](https://www.chromium.org/)
[![Electron](https://img.shields.io/badge/Electron-apps-47848f?style=for-the-badge&logo=electron&logoColor=fff)](https://www.electronjs.org/)
[![zram](https://img.shields.io/badge/zram-zstd-6e4a7e?style=for-the-badge)](https://wiki.archlinux.org/title/Zram)
[![License](https://img.shields.io/badge/license-MIT-0f3460?style=for-the-badge)](./LICENSE)

**588 MiB → 171 MiB. The pages were never touched, so nobody cares.**

---

*"Browsers ask for RAM. The kernel delivers. bssl-ram whispers to the kernel."*

</div>

---

> [!IMPORTANT]
> **bssl-ram is a tiny autonomous daemon that shrinks idle browser tabs and Electron windows by ~70% RSS without the app
noticing.**
> Out of the box it covers Firefox, LibreWolf, Zen, Waterfox, Chrome, Chromium, Brave, Edge, Vivaldi, Opera, Discord,
> Slack, VS Code, Spotify, Obsidian and basically any other Electron-based desktop app. It doesn't restart, discard, or
> reload anything. It just tells the kernel "page this out to zram — the user isn't looking". When the tab comes back, the
> kernel decompresses transparently on page fault. The app never learns this happened.

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
        PROC["/proc/*/cmdline<br/>match browser + electron profiles"]
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

The daemon is driven by a **cn_proc netlink subscription** (the process table is maintained in-memory from kernel fork/exec/exit events — the per-cycle `/proc` walk is gone) plus a single Tokio loop with **two wake sources**: a safety-net timer (`scan_interval_secs`) and an event-driven PSI memory-pressure trigger. When the system is comfortable, the daemon idles between timer ticks and burns essentially zero CPU. When the kernel reports real memory stall (`/proc/pressure/memory` crosses the configured threshold), `poll(POLLPRI)` fires and the daemon scans immediately — no waiting for the next tick. Every `scan_interval_secs` it:

1. Walks `/proc` and matches each cmdline against the configured **profiles**. Firefox tabs use `-isForBrowser ... tab`;
   everything Chromium-based (Chrome, Brave, Edge, Vivaldi, Opera, *and* every Electron app) carries `--type=renderer`.
   Extension renderers (`--extension-process`) and infrastructure procs (gpu/utility/zygote/crashpad/rdd/socket) are
   excluded.
2. Reads `utime + stime` from `/proc/PID/stat` and diffs against the previous snapshot. Targets that burn ≤ 2 ticks (
   20ms CPU) per cycle for 3 consecutive cycles are flagged idle.
3. Parses `/proc/PID/smaps`, selects only **private anonymous** regions (perms `p`, inode 0, `Anonymous: > 0 kB`), and
   batches them through `process_madvise(pidfd, iov, MADV_PAGEOUT)` in chunks of `IOV_MAX=1024`.

That's the whole thing. No ptrace, no signals, no process suspension. The kernel handles decompression on demand — the
app doesn't know its pages moved.

---

## 📊 Benchmarks (v0.3.0)

> [!IMPORTANT]
> Every number below comes from the reproducible suite under [`bench/`](./bench/).
> Re-run any script and your machine produces a comparable report — methodology
> in [`bench/README.md`](./bench/README.md), aggregation + plots via
> `Rscript bench/analyze.R`. **Workload**: author's own idle Firefox + Chromium +
> Electron session (~20 renderer-ish targets). **Kernel**: Linux 6.19.12-zen1-1-zen.

### A — Daemon CPU per discovery mode

300 s sample window per config, `dry_run=true` so successive runs don't perturb
each other through residual zram churn.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/plots/test-a-cpu-dark.png">
  <img alt="Daemon CPU per discovery mode — three bars showing 0.093 percent for /proc walk, 0.083 percent for cn_proc, 0.080 percent for cn_proc plus eBPF" src="bench/results/plots/test-a-cpu-light.png" width="100%">
</picture>

```mermaid
xychart-beta
    title "Daemon CPU % over a 300 s window  (lower is better)"
    x-axis ["procwalk", "cn_proc", "cn_proc + BPF"]
    y-axis "Average CPU %" 0.07 --> 0.10
    bar [0.093, 0.083, 0.080]
```

| Config                         | CPU in 300 s | Average CPU % | vs `/proc` walk |
|:-------------------------------|-------------:|--------------:|----------------:|
| `/proc` walk + `/proc/PID/stat`|       280 ms |   **0.093 %** |              —  |
| cn_proc + `/proc/PID/stat`     |       250 ms |     0.083 %   |        **−11 %**|
| cn_proc + **eBPF** (v0.3.0)    |       240 ms |     0.080 %   |        **−14 %**|

The headline is the absolute floor — even with a busy desktop session the
daemon stays below **0.1 % CPU**. The deltas matter because every TIER-S
feature shaves a bit more off an already tiny budget; on a 24-core box that
~10 % delta is essentially free, but on a Pi-class device it adds up.

### B — Reaction latency under 14 GiB memory pressure

A child Python process allocates 14 GiB and writes one byte per page,
forcing the kernel into reclaim territory. We time from the allocation
starting to the first `page-out done` line in the daemon's log.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/plots/test-b-psi-latency-dark.png">
  <img alt="Reaction latency under memory pressure — PSI on at 3402 ms vs timer-only at 17041 ms" src="bench/results/plots/test-b-psi-latency-light.png" width="100%">
</picture>

```mermaid
xychart-beta
    title "Wall-clock from alloc start to first compress  (lower is better)"
    x-axis ["PSI on", "Timer-only (10 s)"]
    y-axis "milliseconds" 0 --> 18000
    bar [3402, 17041]
```

| Mode                                  | Time to first compress |  vs PSI |
|:--------------------------------------|-----------------------:|--------:|
| `psi_enabled = true`  (event-driven)  |           **3 402 ms** |       — |
| `psi_enabled = false` (timer, 10 s)   |             17 041 ms  |  **5×** |

> [!NOTE]
> The 3.4 s figure is *dominated by the Python allocation ramp itself*
> (~3 s to fault in 14 GiB and accumulate 50 ms of stall). The kernel→daemon
> wakeup once `some_avg10` crosses the threshold is sub-millisecond — see
> `bench/results/psi-latency-*.log` for the raw timestamps.

### C — Real compression on the largest renderer

`bench/scripts/bench-real-compress.sh` picks the biggest live target,
issues exactly one `process_madvise(MADV_PAGEOUT)` sweep across all of
its private-anonymous regions, and captures `/proc/PID/smaps_rollup`
on either side.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/plots/test-c-rss-before-after-dark.png">
  <img alt="RSS before vs after compression — drops from 300 MiB to 191 MiB" src="bench/results/plots/test-c-rss-before-after-light.png" width="80%">
</picture>

| Metric           |  Before |   After |                                Δ |
|:-----------------|--------:|--------:|---------------------------------:|
| **RSS**          | 300 MiB | 191 MiB |          **−109 MiB (−36 %)**    |
| **PSS**          | 162 MiB |  50 MiB |                       −112 MiB   |
| **Swap (zram)**  |  13 MiB | 122 MiB |              **+109 MiB**        |
| **Anonymous**    | 128 MiB |  19 MiB |                       −109 MiB   |
| **Syscall time** |       — |       — | **398 ms** — 669 regions, 1 batch|

Chrome never noticed. The tab kept rendering; the only user-visible
effect on the next switch-to was a faint page-fault ramp as zram
streamed the working set back in.

### E — Recompression cascade prevention

90 s aggressive window: `idle_cycles_threshold = 1`,
`scan_interval_secs = 5`, so every target becomes eligible for compression
every 5 s. Pre-fix browsers' GC pulses flipped the anti-recompression flag
and the daemon paged the same PIDs out multiple times inside the window —
the bug Rodrigo flagged in `c0acaf3`.

<table>
<tr>
<td>

```mermaid
%%{init: {'theme':'base', 'themeVariables': {'pie1':'#3a8d3a','pie2':'#e15759'}}}%%
pie showData
    title "v0.3.0 — same 90 s window"
    "Unique compressions" : 14
    "Recompressions (waste)" : 0
```

</td>
<td>

```mermaid
%%{init: {'theme':'base', 'themeVariables': {'pie1':'#3a8d3a','pie2':'#e15759'}}}%%
pie showData
    title "v0.1.x — pre-dual-threshold"
    "Unique compressions" : 6
    "Recompressions (waste)" : 3
```

</td>
</tr>
</table>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="bench/results/plots/test-e-recompression-dark.png">
  <img alt="Recompression cascade prevention — 14 unique compressions, 0 recompressions" src="bench/results/plots/test-e-recompression-light.png" width="80%">
</picture>

| Metric                 |       v0.3.0 | v0.1.x (re-run pre-fix) |
|:-----------------------|-------------:|------------------------:|
| Total compress events  |       **14** |                       9 |
| Unique PIDs compressed |       **14** |                       6 |
| Recompressions         |        **0** |                       3 |
| Recompression rate     |     **0.0 %**|                  33.3 % |

v0.3.0 compressed every eligible target exactly once. The pre-fix number
is from the live repro we did before landing the dual threshold.

<details>
<summary>📂 Where the raw data lives</summary>

Each script under `bench/scripts/` writes a timestamped `*.txt` summary
+ a `*.log` with the full daemon output to `bench/results/`. Re-running
any script appends a new run; `Rscript bench/analyze.R` aggregates
across all of them and re-renders the plots.

```
bench/
├── analyze.R                       # parser + ggplot renderer
├── configs/                        # one TOML per test variant
├── scripts/                        # one runner per test
└── results/
    ├── cpu-<timestamp>.txt          (Test A)
    ├── psi-latency-<timestamp>.txt  (Test B)
    ├── compress-real-<timestamp>.txt (Test C)
    ├── recompress-<timestamp>.txt   (Test E)
    └── plots/
        ├── test-a-cpu-{light,dark}.png
        ├── test-b-psi-latency-{light,dark}.png
        ├── test-c-rss-before-after-{light,dark}.png
        └── test-e-recompression-{light,dark}.png
```

</details>

### Historical reference — a bigger tab

Earlier development run, kept here because it pins the "how far can it
go in one shot" envelope — a 588 MiB Firefox tab gave back **−417 MiB
(−70 % RSS)** with **+374 MiB** of zram growth (~260 MiB real RAM
returned to the system after zstd compression).

---

## ⚙️ Configuration

`/etc/bssl-ram/config.toml` — all fields optional, defaults shown:

```toml
scan_interval_secs = 10   # seconds between /proc scans (safety-net cap when PSI is on)
idle_cycles_threshold = 3    # consecutive idle cycles before compressing (3 × 10s = 30s)
cpu_delta_threshold = 2    # CPU ticks per cycle to be considered idle (2 ticks = 20ms)
wakeup_delta_threshold = 50  # CPU ticks/cycle that count as a real user wakeup (≥ 500ms CPU)
min_rss_mib = 50   # don't bother compressing tiny processes
dry_run = false

# PSI memory pressure trigger -------------------------------------------------
# When enabled, the daemon also wakes up immediately whenever the kernel
# reports `psi_stall_threshold_us` of cumulative memory stall inside any
# rolling `psi_window_us` window. Idle systems → near-zero CPU; pressure
# spikes → reaction in the same cycle. Requires CAP_SYS_RESOURCE
# (granted by the systemd unit). On any failure (kernel without
# CONFIG_PSI, missing cap, …) the daemon logs a warning and silently
# falls back to timer-only mode.
psi_enabled            = true
psi_stall_threshold_us = 150000   # 150 ms of "some-tasks-stalled"
psi_window_us          = 1000000  # ... within any 1 s window

# Profiles are how the scanner decides what counts as a "compressible
# target". The defaults below cover Firefox-family + Chromium-family +
# every Electron app — you only need this section if you want to add new
# match rules or replace the defaults.
#
# [[profiles]]
# name = "my-app"
# binary_substring_any = ["myapp"]   # case-insensitive substrings of argv[0]
# arg_required_all     = ["--worker"]
# arg_excluded_any     = ["--debug"]
# arg_last             = "tab"
```

### Supported apps (built-in profiles)

| Profile    | Matches                                                                                                                                                                                                                                                                                |
|:-----------|:---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `firefox`  | Firefox, LibreWolf, Zen Browser, Waterfox, IceCat — any tab content process (`-isForBrowser ... tab`)                                                                                                                                                                                  |
| `chromium` | Chrome, Chromium, Brave, Edge, Vivaldi, Opera, Yandex, Thorium, **and every Electron app** (VS Code, Discord, Slack, Spotify, Obsidian, Signal, Notion, Element, Teams, Vesktop, …) — any `--type=renderer` content process. Extension renderers (`--extension-process`) are excluded. |

---

## 🧪 Development

The daemon ships with four inspection examples that bypass the main loop and let you validate each subsystem in
isolation:

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

| Requirement                       | Why                                                                                |
|:----------------------------------|:-----------------------------------------------------------------------------------|
| Linux kernel ≥ 5.10               | `process_madvise` and `pidfd_open` syscalls                                        |
| zram configured as swap           | Without it, pages go to disk — defeats the point                                   |
| `CAP_SYS_NICE` + `CAP_SYS_PTRACE` | Granted by the systemd unit or via `setcap` — no permanent root                    |
| At least one supported app        | Firefox, any Chromium-based browser, or any Electron app (see profile table above) |

---

## 🚀 Push it further — kernel-side free wins

bssl-ram doesn't replace what the kernel already does well; it stacks on top. Two zero-code knobs amplify everything the daemon does.

### MGLRU (Multi-Generational LRU) — better aging, less kswapd

Linux 6.1+ ships an alternative page-reclaim algorithm that uses generations instead of the binary active/inactive lists. Google's fleet data: **40% less kswapd CPU, 85% fewer low-memory kills at the 75th percentile**. It's compiled in but disabled by default on most distros.

```bash
# Enable all three components (base + leaf-PTE + non-leaf-PTE access bit clearing)
echo y | sudo tee /sys/kernel/mm/lru_gen/enabled

# Anti-thrashing TTL — protects working set from premature eviction
echo 1000 | sudo tee /sys/kernel/mm/lru_gen/min_ttl_ms

# Confirm
cat /sys/kernel/mm/lru_gen/enabled    # should print 0x0007
```

Persist across boots via a systemd-tmpfiles drop-in or sysfs.d snippet. Reference: [`Documentation/admin-guide/mm/multigen_lru.rst`](https://docs.kernel.org/admin-guide/mm/multigen_lru.html).

### zram multi-algorithm + recompression — squeeze the last drops

Default zram uses one fast algorithm (zstd or lz4). You can stack a fast primary for write latency with a slow-but-strong secondary for already-cold pages — typical result is **4–5× compression ratio** instead of the usual 2–3×.

```ini
# /etc/systemd/zram-generator.conf
[zram0]
zram-size = ram
compression-algorithm = lzo-rle zstd(level=15)
# Optional: spill the genuinely incompressible pages to a raw partition
# writeback-device = /dev/disk/by-id/<your-nvme>-partN
```

Then a tiny script re-compresses idle pages with the secondary algorithm in the background. Reference: [systemd-zram-generator multi-comp recipe](https://gist.github.com/Szpadel/9a1960e52121e798a240a9b320ec13c8) and [`Documentation/admin-guide/blockdev/zram.rst`](https://docs.kernel.org/admin-guide/blockdev/zram.html).

These two changes together stretch the daemon's per-page payoff: you compress more bytes per RAM byte saved, evicted pages stay accurate to the actual working set, and the kernel does less hot-path work to keep up.

---

## 🧯 Known limitations

- **No per-tab granularity.** Browsers group same-site tabs into one process (Fission in Firefox, site-per-process in
  Chromium) — compressing one compresses all siblings. Acceptable since they'll all idle together.
- **Background media detection.** A tab playing audio through MSE may show low CPU delta because the actual decoding
  happens in a sibling decoder process. Future work: a D-Bus MPRIS listener to globally block compression during
  `PlaybackStatus=Playing`.
- **WebRTC / Meet / Zoom.** These rarely expose `MediaSession`, so MPRIS won't help. Future work: a minimal Native
  Messaging Host as a cooperative "please don't compress" signal from the page.
- **Cold-start latency.** The first access after compression pays a page-fault roundtrip plus zstd decompression (
  sub-100ms for typical working sets — noticeable but not painful).

---


<div align="center">

**`bssl` — browsers should suckless.**

</div>
