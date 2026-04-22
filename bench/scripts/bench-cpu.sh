#!/usr/bin/env bash
#
# Test A — daemon CPU consumption per discovery mode.
#
# Three back-to-back runs of the daemon (each 5 minutes) with
# different feature sets:
#
#   A1  /proc walk + /proc/PID/stat   (v0.1.x baseline path)
#   A2  cn_proc table + /proc/PID/stat (v0.2.0 path)
#   A3  cn_proc table + BPF map        (v0.3.0 hot path)
#
# CPU is sampled by reading utime+stime from /proc/<pid>/stat at
# 2s and 302s after launch — the 300s window keeps the signal
# above the 1-tick (10ms) sampling resolution. dry_run is on so
# the test only measures discovery/scheduling overhead.
#
# Output:  bench/results/cpu-<timestamp>.txt
# Stdout:  one summary line per config + final markdown table.

set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO/daemon/target/release/bssl-ram"
CONFIGS="$REPO/bench/configs"
RESULTS="$REPO/bench/results"
SAMPLE_S=${SAMPLE_S:-300}        # configurable for quick smoke runs (e.g. SAMPLE_S=60 ./bench-cpu.sh)

if [[ ! -x "$BIN" ]]; then
    echo "binary not found at $BIN — run 'cargo build --release' in daemon/ first" >&2
    exit 1
fi

stamp=$(date +%Y%m%d-%H%M%S)
out="$RESULTS/cpu-$stamp.txt"
mkdir -p "$RESULTS"

TICK_HZ=$(getconf CLK_TCK)

# Run one variant: copy config, set caps, launch, sample CPU, kill.
# args: <label> <config_basename> <caps_string>
run_variant() {
    local label="$1" config="$2" caps="$3"
    sudo cp "$CONFIGS/$config" /etc/bssl-ram/config.toml
    sudo setcap "$caps" "$BIN"

    BSSL_LOG_FORMAT=compact RUST_LOG=warn "$BIN" >/dev/null 2>&1 &
    local pid=$!
    sleep 2
    local start_ticks
    start_ticks=$(awk '{print $14+$15+$16+$17}' "/proc/$pid/stat" 2>/dev/null || echo 0)

    sleep "$SAMPLE_S"

    local end_ticks
    end_ticks=$(awk '{print $14+$15+$16+$17}' "/proc/$pid/stat" 2>/dev/null || echo 0)
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true

    local delta=$(( end_ticks - start_ticks ))
    local cpu_ms
    cpu_ms=$(awk -v t="$delta" -v hz="$TICK_HZ" 'BEGIN { printf "%.0f", t * 1000 / hz }')
    local pct
    pct=$(awk -v ms="$cpu_ms" -v window="$SAMPLE_S" 'BEGIN { printf "%.4f", ms / (window * 1000) * 100 }')
    printf '%s | ticks=%-4d | cpu_ms=%-5s | cpu%%=%s\n' "$label" "$delta" "$cpu_ms" "$pct" \
        | tee -a "$out"
}

{
    echo "================================================================"
    echo "Test A — daemon CPU per discovery mode  (sample window: ${SAMPLE_S}s)"
    echo "Started:  $(date -Iseconds)"
    echo "Targets:  $(grep -cE 'firefox|chrome' /proc/*/comm 2>/dev/null | wc -l) browser/electron procs visible"
    echo "Kernel:   $(uname -r)"
    echo "================================================================"
    echo ""
} | tee "$out"

run_variant "A1-procwalk      " A1-procwalk.toml \
    "cap_sys_nice,cap_sys_ptrace,cap_sys_resource+eip"

run_variant "A2-cnproc        " A2-cnproc.toml \
    "cap_sys_nice,cap_sys_ptrace,cap_sys_resource,cap_net_admin+eip"

run_variant "A3-cnproc-bpf    " A3-cnproc-bpf.toml \
    "cap_sys_nice,cap_sys_ptrace,cap_sys_resource,cap_net_admin,cap_bpf,cap_perfmon+eip"

sudo rm -f /etc/bssl-ram/config.toml
echo ""
echo "Saved: $out"
