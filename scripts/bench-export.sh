#!/usr/bin/env bash
# Export a saved criterion baseline to the JSON that scripts/bench-compare.sh reads.
#
# Usage: bash scripts/bench-export.sh [--target-dir DIR] [--sha SHA] [--machine-txt FILE]
#                                     BASELINE OUT.json
#   --target-dir DIR    CARGO_TARGET_DIR the baseline was saved in (default: ./target)
#   --sha SHA           benched commit (default: the one bench-run.sh recorded, else HEAD)
#   --machine-txt FILE  also write the machine info as plain text (CPU, governor, rustc -V, SHA)
# OUT.json holds {schema, baseline, sha, created, machine, benches: {<full id>: {mean, median,
# std_dev, mean_ci, median_ci}}}: point estimates and criterion's [lower, upper] confidence
# interval (95 % by default), in criterion's unit (ns for wall time).
# Requires: jq, find.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() { sed -n '2,11p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }

target="${CARGO_TARGET_DIR:-$ROOT/target}" sha="" machine_txt=""
args=()
while (($#)); do
    case "$1" in
        --target-dir) target="${2:?--target-dir needs a directory}"; shift 2 ;;
        --sha) sha="${2:?--sha needs a commit}"; shift 2 ;;
        --machine-txt) machine_txt="${2:?--machine-txt needs a file}"; shift 2 ;;
        -h | --help) usage ;;
        -*) echo "bench-export: unknown argument: $1" >&2; usage ;;
        *) args+=("$1"); shift ;;
    esac
done
((${#args[@]} == 2)) || usage
baseline="${args[0]}" out="${args[1]}"

for tool in jq find; do
    command -v "$tool" >/dev/null || { echo "bench-export: missing $tool" >&2; exit 2; }
done

criterion="$target/criterion"
[[ -d "$criterion" ]] || { echo "bench-export: no $criterion" >&2; exit 2; }

if [[ -z "$sha" ]]; then
    if [[ -f "$criterion/.bench-run/$baseline.sha" ]]; then
        sha="$(<"$criterion/.bench-run/$baseline.sha")"
    else
        sha="$(git rev-parse HEAD)"
        echo "bench-export: warning: no recorded SHA for '$baseline', using HEAD" >&2
    fi
fi

cpu="$(awk -F': *' '/^model name/ { print $2; exit }' /proc/cpuinfo 2>/dev/null || true)"
governor="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
rustc_v="$(rustc -V 2>/dev/null || echo unknown)"
kernel="$(uname -sr)"
cpus="$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo unknown)"

# One {id: {mean, median, std_dev, mean_ci, median_ci}} object per benchmark saved under this baseline.
mapfile -t dirs < <(find "$criterion" -type f -path "*/$baseline/benchmark.json" -printf '%h\n' | sort)
((${#dirs[@]})) || { echo "bench-export: no benchmarks saved as baseline '$baseline' in $criterion" >&2; exit 1; }

benches="$(
    for d in "${dirs[@]}"; do
        [[ -f "$d/estimates.json" ]] || continue
        jq -n --slurpfile b "$d/benchmark.json" --slurpfile e "$d/estimates.json" \
            '{($b[0].full_id): {mean: $e[0].mean.point_estimate, median: $e[0].median.point_estimate,
              std_dev: $e[0].std_dev.point_estimate,
              mean_ci: [$e[0].mean.confidence_interval.lower_bound, $e[0].mean.confidence_interval.upper_bound],
              median_ci: [$e[0].median.confidence_interval.lower_bound, $e[0].median.confidence_interval.upper_bound]}}'
    done | jq -s 'add // {}'
)"

mkdir -p "$(dirname "$out")"
jq -n --tab --arg baseline "$baseline" --arg sha "$sha" --arg created "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg cpu "${cpu:-unknown}" --arg governor "$governor" --arg rustc "$rustc_v" \
    --arg kernel "$kernel" --arg cpus "$cpus" --argjson benches "$benches" \
'{schema: 1, baseline: $baseline, sha: $sha, created: $created,
      machine: {cpu: $cpu, cpus: $cpus, governor: $governor, rustc: $rustc, kernel: $kernel},
      benches: ($benches | to_entries | sort_by(.key) | from_entries)}' >"$out"

if [[ -n "$machine_txt" ]]; then
    mkdir -p "$(dirname "$machine_txt")"
    printf 'cpu: %s\ncpus: %s\ngovernor: %s\nrustc: %s\nkernel: %s\nsha: %s\nbaseline: %s\n' \
        "${cpu:-unknown}" "$cpus" "$governor" "$rustc_v" "$kernel" "$sha" "$baseline" >"$machine_txt"
fi
echo "bench-export: $(jq '.benches | length' "$out") benches from baseline '$baseline' @ ${sha:0:12} -> $out"
