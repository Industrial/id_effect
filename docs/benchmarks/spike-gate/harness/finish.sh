#!/usr/bin/env bash
cd /data/Code/rust/id_effect
OUT=docs/benchmarks/spike-gate
bash scripts/bench-export.sh --target-dir .tmp/spike-gate/probe/target --sha "$(git rev-parse HEAD)" probe-r1 "$OUT/json/probe-r1.json" >/dev/null
sed -i 's/ -> exit=0 / -> /' "$OUT/probe-stack.txt"
sed -i 's/^# impl kind n kib -> result.*/# impl kind n kib -> result (release profile, as cargo bench builds)/' "$OUT/probe-stack.txt"
mkdir -p "$OUT/harness/probe/src" "$OUT/harness/probe/benches"
cp .tmp/spike-gate/probe/Cargo.toml "$OUT/harness/probe/Cargo.toml"
cp .tmp/spike-gate/probe/src/lib.rs .tmp/spike-gate/probe/src/main.rs "$OUT/harness/probe/src/"
cp .tmp/spike-gate/probe/benches/probe.rs "$OUT/harness/probe/benches/"
for s in mkbranch.sh round.sh driver.sh launch.sh aggregate.sh export-baseline.sh probes.sh checktime.sh checktime2.sh verify.sh gatecmd.sh annotations.sh; do
    cp ".tmp/spike-gate/$s" "$OUT/harness/$s"
done
jq -r '.benches | to_entries[] | "\(.key) \(.value.median)"' "$OUT/json/probe-r1.json"
