#!/usr/bin/env bash
# Referee probes: laziness, allocation counts, stack depth (2 MiB thread) for v0.6, A, B.
set -uo pipefail
cd /data/Code/rust/id_effect
OUT=docs/benchmarks/spike-gate
mkdir -p "$OUT"
P=.tmp/target-probe/release/spike_gate_probe
{
    echo "# probe lazy  ($(date -u +%FT%TZ))"
    "$P" lazy
    echo
    echo "# probe allocs (allocations + bytes for ONE call, after one warm-up call)"
    "$P" allocs
} >"$OUT/probe-lazy-allocs.txt" 2>&1
{
    echo "# stack probe: left-nested chain built then run on a thread with the given stack (KiB)"
    echo "# impl kind n kib -> result (exit code; 134/139 = abort/segv = stack overflow)"
    for kib in 2048 8192 65536; do
        for imp in v06 a b; do
            for kind in async sync; do
                for n in 10000 100000; do
                    res=$("$P" stack "$imp" "$kind" "$n" "$kib" 2>&1 | tail -1)
                    code=$?
                    code=${PIPESTATUS[0]}
                    echo "$imp $kind $n $kib -> exit=$code ${res:0:120}"
                done
            done
        done
    done
} >"$OUT/probe-stack.txt" 2>&1
cat "$OUT/probe-lazy-allocs.txt" "$OUT/probe-stack.txt"
