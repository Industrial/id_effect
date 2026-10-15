#!/usr/bin/env bash
# Growth point: check-time at 32 caps x 400 handlers, 5 runs, once per template.
set -uo pipefail
cd /data/Code/rust/id_effect
OUT=docs/benchmarks/spike-gate
export ID_EFFECT_PATH=$PWD/.tmp/bench-v0.6/crates/id_effect
f=$OUT/check-time-32x400.jsonl
: >"$f"
for t in plain v06 a b; do
    echo "32x400 template=$t load=$(cut -d' ' -f1 /proc/loadavg) rustc=$(pgrep -c -x rustc || true)" >>"$OUT/check-time.log"
    bash spikes/scripts/check-time.sh --gen "$t" 32 400 --runs 5 --json >>"$f" 2>>"$OUT/check-time.log"
done
cat "$f"
