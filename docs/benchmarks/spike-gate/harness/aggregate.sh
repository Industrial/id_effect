#!/usr/bin/env bash
# Export every round baseline and build the median-of-rounds table.
# Usage: aggregate.sh N_ROUNDS
set -euo pipefail
cd /data/Code/rust/id_effect
n=$1
OUT=docs/benchmarks/spike-gate
J=$OUT/json
mkdir -p "$J"
sha_a=$(git log -1 --format=%H -- spikes/kernel_a)
sha_b=$(git log -1 --format=%H -- spikes/kernel_b)
for r in $(seq 1 "$n"); do
    bash scripts/bench-export.sh --target-dir .tmp/target-bench-v06 "v06-r$r" "$J/v06-r$r.json" >/dev/null
    bash scripts/bench-export.sh --target-dir .tmp/target-spikes --sha "$sha_a" "a-r$r" "$J/a-r$r.json" >/dev/null
    bash scripts/bench-export.sh --target-dir .tmp/target-spikes --sha "$sha_b" "b-r$r" "$J/b-r$r.json" >/dev/null
done
# median-of-rounds export per implementation, in bench-export format (bench-compare.sh reads it)
for impl in v06 a b; do
    files=()
    for r in $(seq 1 "$n"); do files+=("$J/$impl-r$r.json"); done
    jq -s --tab --arg impl "$impl" '
      def med: sort | if length % 2 == 1 then .[(length-1)/2] else (.[length/2-1] + .[length/2]) / 2 end;
      . as $all
      | (.[0] | del(.benches)) + {baseline: ($impl + "-median-of-rounds"), rounds: length,
        benches: ([.[].benches | keys[]] | unique | map(. as $id | {key: $id, value: {
          median: ([$all[] | .benches[$id].median // empty] | med),
          mean: ([$all[] | .benches[$id].mean // empty] | med),
          std_dev: ([$all[] | .benches[$id].std_dev // empty] | med),
          round_medians: [$all[] | .benches[$id].median // empty]}}) | from_entries)}
    ' "${files[@]}" >"$J/$impl-median.json"
done
for impl in a b; do
    rc=0
    bash scripts/bench-compare.sh "$J/v06-median.json" "$J/$impl-median.json" >"$OUT/compare-$impl.txt" 2>&1 || rc=$?
    echo "bench-compare v06-median vs $impl-median: exit=$rc" >>"$OUT/compare-$impl.txt"
done
jq -r -n --slurpfile v "$J/v06-median.json" --slurpfile a "$J/a-median.json" --slurpfile b "$J/b-median.json" '
  ([$v[0].benches, $a[0].benches, $b[0].benches] | map(keys) | add | unique)[] as $id
  | [$id, ($v[0].benches[$id].median // null), ($a[0].benches[$id].median // null), ($b[0].benches[$id].median // null),
     ($v[0].benches[$id].round_medians // []), ($a[0].benches[$id].round_medians // []), ($b[0].benches[$id].round_medians // [])]
  | @json' >"$OUT/table.jsonl"
cat "$OUT/table.jsonl"
