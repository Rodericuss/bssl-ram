# bssl-ram (browsers should suckless ram)

Compresses idle Firefox tab memory using `process_madvise(MADV_PAGEOUT)` + zram.

## How it works

The daemon scans `/proc` every 10 seconds looking for Firefox tab processes
(`-isForBrowser ... tab` in cmdline). It tracks CPU usage delta per process —
if a process has been genuinely idle (near-zero CPU) for 3 consecutive cycles,
it tells the kernel to page its memory out to zram (compressed RAM).

When you return to a tab, the kernel transparently decompresses pages on demand
via normal page faults. No reload, no data loss, no process restart.

```
[daemon]
  ↓ scans every 10s
/proc/*/cmdline  →  finds Firefox tab processes (-isForBrowser ... tab)
/proc/PID/stat   →  CPU delta per process
  ↓ if idle for 3+ cycles AND RSS > 50MiB
process_madvise(MADV_PAGEOUT)
  ↓
[zram: compressed in RAM, ~3:1 ratio]
  ↓ on next page fault (tab becomes active)
[kernel decompresses transparently]
```

## Requirements

- Linux kernel ≥ 5.10 (`process_madvise` + `pidfd_open`)
- Firefox (any recent version)
- zram configured as swap (strongly recommended)
- Root or `CAP_SYS_PTRACE` capability

## Setup

### 1. zram (one-time, Arch Linux)

```bash
sudo pacman -S zram-generator
```

Or manually:
```bash
sudo modprobe zram
echo lz4 | sudo tee /sys/block/zram0/comp_algorithm
echo 4G  | sudo tee /sys/block/zram0/disksize
sudo mkswap /dev/zram0
sudo swapon /dev/zram0
```

### 2. Daemon

```bash
cd daemon
cargo build --release
sudo ./target/release/bssl-ram
```

Dry-run (logs only, no compression):
```bash
# in /etc/bssl-ram/config.toml:
dry_run = true
```

### 3. Extension (optional)

Provides idle detection logging for debugging.
Load `extension/` as a temporary add-on via `about:debugging`.

## Configuration

`/etc/bssl-ram/config.toml`:

```toml
scan_interval_secs   = 10   # seconds between /proc scans
idle_cycles_threshold = 3   # idle cycles before compressing (3×10s = 30s)
cpu_delta_threshold  = 2    # CPU ticks delta considered idle (2 ticks = 20ms)
min_rss_mib          = 50   # skip processes using less than this
dry_run              = false
```

## Project structure

```
bssl-ram/
├── daemon/src/
│   ├── main.rs        entry point + main scan loop
│   ├── config.rs      TOML config
│   ├── scanner.rs     finds Firefox tab processes in /proc
│   ├── compressor.rs  process_madvise + smaps parsing + cpu ticks
│   ├── state.rs       CPU delta tracker per PID
│   └── zram.rs        zram setup check
└── extension/
    ├── manifest.json
    ├── background.js  idle detection + debug logging
    └── content.js     visibility state reporter
```

## Known limitations

- Compresses all idle Firefox tab processes — no per-tab granularity
- Cannot distinguish a YouTube tab playing audio from a truly idle tab
  (both may have low CPU delta if audio decoding is in the rdd process)
- Requires root or CAP_SYS_PTRACE for process_madvise on other processes
