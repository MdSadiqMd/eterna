#!/usr/bin/env bash
# Generate a flamegraph for the bench workload and store it with incremental naming.
set -euo pipefail

cd "$(dirname "$0")"
mkdir -p flamegraphs

n=1
while [ -f "flamegraphs/$(printf '%02d' "$n").svg" ]; do
    n=$((n + 1))
done
OUT="flamegraphs/$(printf '%02d' "$n").svg"

echo "→ building bench (flamegraph profile)"
cargo build --profile flamegraph --bin bench 2>&1

echo "→ profiling — output: $OUT"
cargo flamegraph \
    --profile flamegraph \
    --bin bench \
    --output "$OUT" \
    -- 2>&1

echo "→ saved $OUT"
