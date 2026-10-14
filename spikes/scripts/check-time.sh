#!/usr/bin/env bash
# Measure `cargo check` wall time of one crate: the median of N runs (default 3).
#
# Usage:
#   spikes/scripts/check-time.sh CRATE_DIR [--runs N] [--json]
#   spikes/scripts/check-time.sh --gen TEMPLATE CAPS HANDLERS [--runs N] [--json]
#   spikes/scripts/check-time.sh --self-test
#
# --gen first runs gen-caps.sh CAPS HANDLERS --template TEMPLATE (default --out), then measures the
# generated crate, e.g. the spike gate's figure: check-time.sh --gen a 16 100.
#
# Method: one untimed warm-up `cargo check` builds the dependencies; then each timed run touches
# the crate's own sources and re-runs `cargo check` with CARGO_INCREMENTAL=0, so every run
# re-type-checks the whole crate and nothing else. Only the crate under test is re-checked; its
# dependencies come from the warm-up. The crate uses its own target dir (CRATE_DIR/target) unless
# CARGO_TARGET_DIR is set. Run it on an idle machine; compare figures only within one session.
#
# Output: `check-time: crate=<name> runs=<n> times_s=<t1,t2,..> median_s=<m> noop_s=<o> rustc=<v>`,
# or the same as one JSON object with --json. noop_s is one untouched `cargo check` (cargo's own
# overhead, already inside every timed run); subtract it when comparing sub-second figures.
# Exit status: 0 ok, 1 a cargo check failed, 2 usage error.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed -E 's/^# ?//'
}

die() {
    echo "check-time: $*" >&2
    exit 2
}

now_ns() {
    date +%s%N
}

median() {
    sort -g | awk '{ v[NR] = $1 } END { if (NR % 2) printf "%.3f", v[(NR + 1) / 2]; else printf "%.3f", (v[NR / 2] + v[NR / 2 + 1]) / 2 }'
}

self_test() {
    local tmp out
    tmp="$(mktemp -d)"
    trap 'rm -rf "${tmp}"' RETURN
    "${SCRIPT_DIR}/gen-caps.sh" 2 4 --out "${tmp}/crate" >/dev/null
    out="$("${BASH_SOURCE[0]}" "${tmp}/crate" --runs 3)"
    if [[ ! "${out}" =~ ^check-time:\ crate=gen_caps_plain_2x4\ runs=3\ times_s=[0-9.]+,[0-9.]+,[0-9.]+\ median_s=[0-9.]+\ noop_s=[0-9.]+\ rustc= ]]; then
        echo "check-time self-test FAILED: unexpected output: ${out}" >&2
        return 1
    fi
    out="$("${BASH_SOURCE[0]}" "${tmp}/crate" --runs 1 --json)"
    if [[ ! "${out}" =~ ^\{\"crate\":\ \"gen_caps_plain_2x4\",\ \"runs\":\ \[[0-9.]+\],\ \"median_s\":\ [0-9.]+,\ \"noop_s\":\ [0-9.]+, ]]; then
        echo "check-time self-test FAILED: unexpected json: ${out}" >&2
        return 1
    fi
    if [[ "$(printf '3\n1\n2\n' | median)" != 2.000 || "$(printf '4\n1\n2\n3\n' | median)" != 2.500 ]]; then
        echo "check-time self-test FAILED: median" >&2
        return 1
    fi
    printf 'pub fn broken() -> u32 { "no" }\n' >"${tmp}/crate/src/lib.rs"
    if "${BASH_SOURCE[0]}" "${tmp}/crate" --runs 1 >/dev/null 2>&1; then
        echo "check-time self-test FAILED: a failing cargo check must exit 1" >&2
        return 1
    fi
    echo "check-time self-test: ok"
}

runs=3
json=0
crate=""
gen=()
while (($# > 0)); do
    case "$1" in
        --self-test)
            self_test
            exit
            ;;
        --runs)
            runs="${2:?--runs needs a number}"
            shift
            ;;
        --json) json=1 ;;
        --gen)
            (($# >= 4)) || die "--gen needs TEMPLATE CAPS HANDLERS"
            gen=("$2" "$3" "$4")
            shift 3
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*) die "unknown option $1" ;;
        *)
            [[ -z "${crate}" ]] || die "unexpected argument $1"
            crate=$1
            ;;
    esac
    shift
done
[[ "${runs}" =~ ^[1-9][0-9]*$ ]] || die "--runs must be a positive integer"

if ((${#gen[@]} > 0)); then
    [[ -z "${crate}" ]] || die "pass either CRATE_DIR or --gen, not both"
    gen_out="$("${SCRIPT_DIR}/gen-caps.sh" "${gen[1]}" "${gen[2]}" --template "${gen[0]}")"
    crate="$(printf '%s\n' "${gen_out}" | sed -n 's/^gen-caps: wrote //p')"
    [[ -n "${crate}" ]] || die "gen-caps did not report an output dir"
fi
[[ -n "${crate}" ]] || {
    usage >&2
    exit 2
}
[[ -f "${crate}/Cargo.toml" ]] || die "no Cargo.toml in ${crate}"
crate="$(cd "${crate}" && pwd)"
manifest="${crate}/Cargo.toml"
name="$(sed -n 's/^name *= *"\(.*\)"/\1/p' "${manifest}" | head -n 1)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${crate}/target}"
export CARGO_INCREMENTAL=0

cargo_check() {
    cargo check --quiet --manifest-path "${manifest}" >"${CARGO_TARGET_DIR}/check-time.log" 2>&1 || {
        echo "check-time: cargo check failed for ${name}:" >&2
        cat "${CARGO_TARGET_DIR}/check-time.log" >&2
        exit 1
    }
}

mkdir -p "${CARGO_TARGET_DIR}"
cargo_check
times=()
for ((i = 0; i < runs; i++)); do
    find "${crate}" -path "${CARGO_TARGET_DIR}" -prune -o -name '*.rs' -exec touch {} +
    start=$(now_ns)
    cargo_check
    end=$(now_ns)
    times+=("$(awk -v ns=$((end - start)) 'BEGIN { printf "%.3f", ns / 1e9 }')")
done
med="$(printf '%s\n' "${times[@]}" | median)"
start=$(now_ns)
cargo_check
end=$(now_ns)
noop="$(awk -v ns=$((end - start)) 'BEGIN { printf "%.3f", ns / 1e9 }')"
rustc_v="$(rustc -V)"
joined="$(printf '%s,' "${times[@]}")"
joined="${joined%,}"

if ((json)); then
    printf '{"crate": "%s", "runs": [%s], "median_s": %s, "noop_s": %s, "rustc": "%s", "dir": "%s"}\n' \
        "${name}" "${joined}" "${med}" "${noop}" "${rustc_v}" "${crate}"
else
    printf 'check-time: crate=%s runs=%s times_s=%s median_s=%s noop_s=%s rustc=%s\n' \
        "${name}" "${runs}" "${joined}" "${med}" "${noop}" "${rustc_v}"
fi
