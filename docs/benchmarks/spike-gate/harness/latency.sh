#!/usr/bin/env bash
cd /data/Code/rust/id_effect
OUT=docs/benchmarks/spike-gate/probe-interrupt-latency.txt
{
    echo "# interrupt latency: fork(sleep 10 s) -> interrupt -> exit observed, 200 samples"
    .tmp/target-probe/release/spike_gate_probe latency-a
    echo "# B: its own test (ParkExecutor, debug profile), spikes/kernel_b/tests/kernel.rs:70"
    CARGO_TARGET_DIR=$PWD/.tmp/target-spikes cargo test --manifest-path spikes/Cargo.toml -p kernel_b --test kernel \
        interrupt_latency_of_a_sleeping_fiber -- --nocapture 2>&1 | grep -E 'latency|test result'
    echo "# B: same test, release profile"
    CARGO_TARGET_DIR=$PWD/.tmp/target-spikes cargo test --release --manifest-path spikes/Cargo.toml -p kernel_b --test kernel \
        interrupt_latency_of_a_sleeping_fiber -- --nocapture 2>&1 | grep -E 'latency|test result'
} >"$OUT" 2>&1
cat "$OUT"
