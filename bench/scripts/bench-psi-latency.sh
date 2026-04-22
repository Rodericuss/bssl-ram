#!/usr/bin/env bash
#
# Test B — reaction latency under induced memory pressure.
#
# A child Python process allocates 14 GiB and writes one byte per page
# to force the pages resident, then sleeps. While it ramps up,
# /proc/pressure/memory `some avg10` rises from ~0% to several percent.
#
# We launch the daemon TWICE with the same workload:
#   - psi_enabled = true   → kernel wakes the daemon via POLLPRI
#   - psi_enabled = false  → daemon can only wake on the timer
# and measure the wall-clock gap between the stress starting and the
# first "page-out done" line in the daemon log.
#
# Output:  bench/results/psi-latency-<timestamp>.txt

set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$REPO/daemon/target/release/bssl-ram"
CONFIGS="$REPO/bench/configs"
RESULTS="$REPO/bench/results"
ALLOC_GIB=${ALLOC_GIB:-14}

stamp=$(date +%Y%m%d-%H%M%S)
out="$RESULTS/psi-latency-$stamp.txt"
mkdir -p "$RESULTS"

# Helper: spawn the alloc workload in the background, return its PID.
spawn_pressure() {
    python3 -c "
import time
chunks = []
target = ${ALLOC_GIB} * 1024
for i in range(target):
    b = bytearray(1024 * 1024)
    for j in range(0, len(b), 4096):
        b[j] = (i + j) & 0xFF
    chunks.append(b)
time.sleep(8)
" &
    echo $!
}

# Run one variant. args: <label> <config_basename>
run_variant() {
    local label="$1" config="$2"
    sudo cp "$CONFIGS/$config" /etc/bssl-ram/config.toml
    sudo setcap "cap_sys_nice,cap_sys_ptrace,cap_sys_resource,cap_net_admin,cap_bpf,cap_perfmon+eip" "$BIN"

    local logfile="$RESULTS/psi-latency-${label}-$stamp.log"
    BSSL_LOG_FORMAT=compact RUST_LOG=info "$BIN" >"$logfile" 2>&1 &
    local daemon_pid=$!
    sleep 3   # let it seed the table + register PSI if applicable

    local stress_start_ns
    stress_start_ns=$(date +%s%N)
    local stress_pid
    stress_pid=$(spawn_pressure)

    # Watch the log for first compress event.
    local timeout_s=25
    local elapsed=0
    local first_compress_ns=0
    while (( elapsed < timeout_s )); do
        if grep -q '"page-out done"' "$logfile"; then
            local first_line
            first_line=$(grep -m1 '"page-out done"' "$logfile" | sed 's/\x1b\[[0-9;]*m//g')
            local first_ts
            first_ts=$(echo "$first_line" | grep -oE '^[0-9-]+T[0-9:]+\.[0-9]+Z')
            first_compress_ns=$(date -d "$first_ts" +%s%N 2>/dev/null || echo 0)
            break
        fi
        sleep 0.1
        elapsed=$(awk -v e="$elapsed" 'BEGIN { print e + 0.1 }')
    done

    kill -TERM "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
    kill -TERM "$stress_pid" 2>/dev/null || true
    wait "$stress_pid" 2>/dev/null || true

    if [[ "$first_compress_ns" == "0" ]]; then
        printf '%s | reaction = NO COMPRESS WITHIN %ds\n' "$label" "$timeout_s" | tee -a "$out"
    else
        local lat_ms
        lat_ms=$(awk -v s="$stress_start_ns" -v e="$first_compress_ns" \
            'BEGIN { printf "%.0f", (e - s) / 1000000 }')
        printf '%s | reaction_ms=%-6s | first compress event detected\n' "$label" "$lat_ms" \
            | tee -a "$out"
    fi
}

{
    echo "================================================================"
    echo "Test B — PSI reaction latency  (alloc=${ALLOC_GIB} GiB)"
    echo "Started:  $(date -Iseconds)"
    echo "Kernel:   $(uname -r)"
    echo "================================================================"
} | tee "$out"

run_variant "B-psi-on        " B-psi-on.toml
sleep 5
run_variant "B-psi-off-timer " B-psi-off.toml

sudo rm -f /etc/bssl-ram/config.toml
echo ""
echo "Saved: $out"
