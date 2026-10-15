#!/usr/bin/env bash
# Debug-profile stack probe + re-run of both spikes' trybuild UI and corpus tests (no edits).
set -uo pipefail
cd /data/Code/rust/id_effect
OUT=docs/benchmarks/spike-gate
cargo build --manifest-path .tmp/spike-gate/probe/Cargo.toml --target-dir .tmp/target-probe >/dev/null 2>&1
P=.tmp/target-probe/debug/spike_gate_probe
{
    echo "# stack probe, DEBUG profile (as cargo test/nextest builds), 2 MiB thread"
    for imp in v06 a b; do
        for kind in async sync; do
            for n in 1000 10000; do
                res=$("$P" stack "$imp" "$kind" "$n" 2048 2>&1 | tail -1)
                echo "$imp $kind $n 2048KiB debug -> ${res:0:100}"
            done
        done
    done
} >"$OUT/probe-stack-debug.txt" 2>&1
cat "$OUT/probe-stack-debug.txt"
export CARGO_TARGET_DIR=$PWD/.tmp/target-spikes
for pkg in kernel_a kernel_b; do
    echo "== $pkg ui" | tee -a "$OUT/verify-tests.txt"
    cargo test --manifest-path spikes/Cargo.toml -p "$pkg" --test ui 2>&1 | grep -E '^test |test result|mismatch|EXPECTED|ok$' | tee -a "$OUT/verify-tests.txt"
    echo "== $pkg corpus" | tee -a "$OUT/verify-tests.txt"
    cargo test --manifest-path spikes/Cargo.toml -p "$pkg" --test corpus 2>&1 | grep -E 'test result|FAILED|panicked' | tee -a "$OUT/verify-tests.txt"
done
