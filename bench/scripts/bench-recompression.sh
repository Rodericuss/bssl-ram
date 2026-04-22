#!/usr/bin/env bash
#
# Test E — count recompressions over a 90 s aggressive window.
#
# Drives the daemon with idle_cycles_threshold=1 and scan_interval=5s
# so every target hits the compress path every 5s. We then count:
#
#   total_events   = lines containing 'action="compress"'
#   unique_pids    = distinct pid= values among those lines
#   recompressions = total_events - unique_pids
#
# Pre-fix (v0.1.x) this ratio was ~33% (PIDs hit 3× in 90s). With the
# v0.2.0 dual-threshold guard it should be ≤ 5% — only legitimate
# user-wakeup-then-idle cycles.
#
# Output:  bench/results/recompress-<timestamp>.txt

set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO/daemon/target/release/bssl-ram"
CONFIGS="$REPO/bench/configs"
RESULTS="$REPO/bench/results"
WINDOW_S=${WINDOW_S:-90}

stamp=$(date +%Y%m%d-%H%M%S)
out="$RESULTS/recompress-$stamp.txt"
mkdir -p "$RESULTS"

sudo cp "$CONFIGS/E-aggressive.toml" /etc/bssl-ram/config.toml
sudo setcap "cap_sys_nice,cap_sys_ptrace,cap_sys_resource,cap_net_admin,cap_bpf,cap_perfmon+eip" "$BIN"

logfile="$RESULTS/recompress-events-$stamp.log"
BSSL_LOG_FORMAT=compact RUST_LOG=info "$BIN" >"$logfile" 2>&1 &
pid=$!
sleep "$WINDOW_S"
kill -TERM "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
sudo rm -f /etc/bssl-ram/config.toml

# ANSI-strip then parse.
clean=$(sed 's/\x1b\[[0-9;]*m//g' "$logfile")

total=$(echo "$clean" | grep -c 'action="compress"' || true)
mapfile -t pids < <(echo "$clean" | grep 'action="compress"' | grep -oE 'pid=[0-9]+' | sort -u)
unique=${#pids[@]}
recompress=$(( total - unique ))
ratio_pct=0
if (( total > 0 )); then
    ratio_pct=$(awk -v r="$recompress" -v t="$total" 'BEGIN { printf "%.1f", r / t * 100 }')
fi

{
    echo "================================================================"
    echo "Test E — recompression cascade prevention  (window: ${WINDOW_S}s)"
    echo "Started: $(date -Iseconds)"
    echo "Kernel:  $(uname -r)"
    echo "----------------------------------------------------------------"
    echo "Total compress events  : $total"
    echo "Unique PIDs compressed : $unique"
    echo "Recompressions         : $recompress  (${ratio_pct}% of total)"
    echo "Per-PID breakdown:"
    echo "$clean" | grep 'action="compress"' | grep -oE 'pid=[0-9]+' | sort | uniq -c | sort -rn
    echo "----------------------------------------------------------------"
    echo "Detailed log: $logfile"
} | tee "$out"
