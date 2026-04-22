# bssl-ram (browsers should suckless ram)

Compresses idle Firefox tab memory using `process_madvise(MADV_PAGEOUT)` + zram.

## How it works

A Firefox extension tracks which tabs are idle and their OS process IDs.
Every 10 seconds it reports this to the daemon via a local WebSocket.
The daemon compresses memory of idle tabs by telling the kernel to page them
out to zram (compressed RAM). When you return to a tab, the kernel
decompresses transparently on the next page fault — no reload, no data loss.

```
[Firefox extension]  →  {pid, idle_seconds} via WebSocket  →  [bssl-ram daemon]
                                                                      ↓
                                                          process_madvise(MADV_PAGEOUT)
                                                                      ↓
                                                              [zram: compressed in RAM]
```

## Requirements

- Linux kernel ≥ 5.10 (`process_madvise` + `pidfd`)
- Firefox with `browser.processes` API (requires privileged extension or dev build)
- zram configured as swap

## Setup

### 1. zram (one-time)

```bash
sudo modprobe zram
echo lz4 | sudo tee /sys/block/zram0/comp_algorithm
echo 4G  | sudo tee /sys/block/zram0/disksize
sudo mkswap /dev/zram0
sudo swapon /dev/zram0
```

Or install `systemd-zram-generator` (Arch: `sudo pacman -S zram-generator`).

### 2. Daemon

```bash
cd daemon
cargo build --release
sudo ./target/release/bssl-ram
```

Dry-run mode (no actual compression, just logging):

```bash
FOXRAM_DRY_RUN=true sudo ./target/release/bssl-ram
```

### 3. Extension

Load `extension/` as a temporary extension in Firefox:
`about:debugging` → "This Firefox" → "Load Temporary Add-on" → select `manifest.json`

## Configuration

`/etc/bssl-ram/config.toml`:

```toml
idle_threshold_secs = 60   # compress tabs idle for more than this
min_rss_mib = 50           # ignore processes using less than this
ws_port = 7878             # WebSocket port
dry_run = false            # log only, no actual compression
```

## Project structure

```
bssl-ram/
├── daemon/src/
│   ├── main.rs         entry point
│   ├── config.rs       TOML config
│   ├── server.rs       WebSocket server
│   ├── compressor.rs   process_madvise + smaps parsing
│   ├── zram.rs         zram setup helpers
│   └── state.rs        TabReport types + eligible_pids logic
└── extension/
    ├── manifest.json
    └── background.js   tab tracking + WebSocket client
```

## Known limitations

- `browser.processes` API requires a privileged Firefox extension (work in progress)
- zram must be configured manually or via zram-generator
- Requires root (or `CAP_SYS_PTRACE`) for `process_madvise` on other processes
