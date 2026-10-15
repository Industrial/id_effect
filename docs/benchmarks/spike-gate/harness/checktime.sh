#!/usr/bin/env bash
# check-time at 16 caps x 100 handlers, 7 runs each, all templates interleaved twice; then UI tests.
set -uo pipefail
cd /data/Code/rust/id_effect
OUT=docs/benchmarks/spike-gate
export ID_EFFECT_PATH=$PWD/.tmp/bench-v0.6/crates/id_effect
f=$OUT/check-time.jsonl
: >"$f"
for pass in 1 2; do
    for t in plain v06 a b; do
        echo "pass=$pass template=$t load=$(cut -d' ' -f1 /proc/loadavg) rustc=$(pgrep -c -x rustc || true)" >>"$OUT/check-time.log"
        bash spikes/scripts/check-time.sh --gen "$t" 16 100 --runs 7 --json >>"$f" 2>>"$OUT/check-time.log"
    done
done
cat "$f"
