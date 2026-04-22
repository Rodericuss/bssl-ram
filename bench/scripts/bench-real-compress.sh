#!/usr/bin/env bash
#
# Test C — real RSS reduction on the largest target.
#
# Picks the renderer with the highest RSS, runs the inspection example
# `compress_real` against it, captures the reported deltas, and saves
# them. The example is built alongside the main binary by `cargo build
# --release --examples`.
#
# Output:  bench/results/compress-real-<timestamp>.txt

set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
EXAMPLE="$REPO/daemon/target/release/examples/compress_real"
RESULTS="$REPO/bench/results"

if [[ ! -x "$EXAMPLE" ]]; then
    echo "example not found — run 'cargo build --release --examples' in daemon/" >&2
    exit 1
fi

sudo setcap "cap_sys_nice,cap_sys_ptrace+eip" "$EXAMPLE"

stamp=$(date +%Y%m%d-%H%M%S)
out="$RESULTS/compress-real-$stamp.txt"
mkdir -p "$RESULTS"

{
    echo "================================================================"
    echo "Test C — real compression on largest renderer"
    echo "Started:  $(date -Iseconds)"
    echo "Kernel:   $(uname -r)"
    echo "----------------------------------------------------------------"
    "$EXAMPLE" 2>&1
    echo "----------------------------------------------------------------"
    echo "Saved: $out"
} | tee "$out"
