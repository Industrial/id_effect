#!/usr/bin/env bash
# The w2-spike-gate leaf gate as written in the release plan, against the committed first-pass
# spike exports; status recorded, not enforced.
cd /data/Code/rust/id_effect
OUT=docs/benchmarks/spike-gate/gate-command.txt
: >"$OUT"
for s in a b; do
    echo "\$ bash scripts/bench-compare.sh docs/benchmarks/v0.6.json docs/benchmarks/spike-$s.json" >>"$OUT"
    rc=0
    bash scripts/bench-compare.sh docs/benchmarks/v0.6.json "docs/benchmarks/spike-$s.json" >>"$OUT" 2>&1 || rc=$?
    echo "exit=$rc" >>"$OUT"
    echo >>"$OUT"
done
cat "$OUT"
