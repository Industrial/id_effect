#!/usr/bin/env bash
# b0-baseline-capture: export criterion baseline v0.6 to docs/benchmarks/v0.6{.json,.machine.txt}
# and record the source SHAs.
set -euo pipefail
cd /data/Code/rust/id_effect
bash scripts/bench-export.sh --target-dir .tmp/target-bench-v06 --machine-txt docs/benchmarks/v0.6.machine.txt \
    v0.6 docs/benchmarks/v0.6.json
tag=$(git rev-parse 'v0.6.0^{commit}')
br=$(git rev-parse bench/v0.6)
tree=$(git rev-parse 'bench/v0.6^{tree}')
c1=$(git rev-parse 135ab69) c2=$(git rev-parse a18c4c5) c3=$(git rev-parse 2bcd932)
tmp=$(mktemp)
jq --tab --arg tag "$tag" --arg br "$br" --arg tree "$tree" --arg c1 "$c1" --arg c2 "$c2" --arg c3 "$c3" \
    --arg criterion "$(grep -A1 '^name = "criterion"' .tmp/bench-v0.6/Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')" \
'. + {sources: {ref: "bench/v0.6", commit: $br, tree: $tree, base: {tag: "v0.6.0", commit: $tag},
        applied: [{commit: $c1, leaf: "b0-bench-core", paths: ["crates/id_effect/benches/gate_core.rs", "crates/id_effect/Cargo.toml"]},
                  {commit: $c2, leaf: "b0-bench-fiber", paths: ["crates/id_effect_tokio/benches/gate_fiber.rs", "crates/id_effect_tokio/Cargo.toml"]},
                  {commit: $c3, leaf: "b0-bench-axum", paths: ["crates/id_effect_axum/benches/gate_request.rs", "crates/id_effect_axum/Cargo.toml"]}],
        src_diff_vs_tag: "empty (git diff v0.6.0 bench/v0.6 -- crates/*/src)",
        export: "git archive bench/v0.6 into .tmp/bench-v0.6 (no worktree); Cargo.lock resolved fresh at build time",
        criterion: $criterion}}' docs/benchmarks/v0.6.json >"$tmp"
mv "$tmp" docs/benchmarks/v0.6.json
cat >>docs/benchmarks/v0.6.machine.txt <<EOF
ref: bench/v0.6 = v0.6.0 ($tag) + bench hunks of $c1, $c2, $c3
tree: $tree
target_dir: .tmp/target-bench-v06 (dedicated)
criterion: $(jq -r .sources.criterion docs/benchmarks/v0.6.json)
EOF
cat docs/benchmarks/v0.6.machine.txt
jq -c '{sha, benches: (.benches | map_values(.median))}' docs/benchmarks/v0.6.json
rc=0
bash scripts/bench-compare.sh docs/benchmarks/v0.6.json docs/benchmarks/v0.6.json || rc=$?
echo "self-compare exit=$rc"
