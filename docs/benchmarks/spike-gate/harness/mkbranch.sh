#!/usr/bin/env bash
# Build bench/v0.6 = v0.6.0 + bench-file hunks of 135ab69, a18c4c5, 2bcd932, via plumbing only.
set -euo pipefail
cd /data/Code/rust/id_effect
SP=/data/Code/rust/id_effect/.tmp/spike-gate
export GIT_INDEX_FILE="$SP/bench-v06.index"
rm -f "$GIT_INDEX_FILE"
git read-tree v0.6.0
for c in 135ab69 a18c4c5 2bcd932; do
    git diff "$c^" "$c" > "$SP/$c.patch"
    git apply --cached --check "$SP/$c.patch"
    git apply --cached "$SP/$c.patch"
done
tree=$(git write-tree)
msg="bench(v0.6): v0.6.0 plus the 0.7 gate bench commits

Baseline for D9 (b0-baseline-capture). Tree = v0.6.0 (68d4b02) plus the
bench files and Cargo.toml [[bench]]/dev-dep hunks of:
- 135ab69 test(id_effect): add gate_core benches
- a18c4c5 feat(id_effect_tokio): add gate_fiber benches
- 2bcd932 feat(axum): add gate_request benches
No crates/*/src changes."
sha=$(git commit-tree "$tree" -p "v0.6.0^{commit}" -m "$msg")
unset GIT_INDEX_FILE
rm -f "$SP/bench-v06.index"
git branch bench/v0.6 "$sha"
echo "tree=$tree sha=$sha"
git diff --stat v0.6.0 bench/v0.6 | cat
echo "src diff lines:"; git diff v0.6.0 bench/v0.6 -- 'crates/*/src' | wc -l
