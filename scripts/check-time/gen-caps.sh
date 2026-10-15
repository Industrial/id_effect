#!/usr/bin/env bash
# Generate a compile-time stress crate with CAPS capabilities and HANDLERS handlers.
#
# Usage:
#   scripts/check-time/gen-caps.sh CAPS HANDLERS [--template NAME|FILE] [--out DIR] [--dry-run]
#   scripts/check-time/gen-caps.sh --self-test [--template NAME|FILE]
#
# Templates (how the handlers are spelled):
#   plain  no effect kernel, plain Rust over a struct env: the floor (default)
#   v06    id_effect 0.6 (`Env` + `Needs<T>`); set ID_EFFECT_PATH to measure a v0.6 export, e.g.
#          ID_EFFECT_PATH=.tmp/bench-v0.6/crates/id_effect (default: this checkout's crates/id_effect)
#   next   the 0.7 kernel, `id_effect::next` (`caps_next!` + `effect_next!`; this checkout's crate
#          unless ID_EFFECT_PATH is set). The spike templates `a` / `b` went with the spikes
#          (git history at 9cf380e).
#   FILE   any file implementing the template interface below
#
# Template interface: a bash file that gen-caps.sh sources, defining six functions that write Rust
# (or TOML) to stdout: tpl_dependencies (the generated crate's [dependencies] lines), tpl_prelude
# (`use` items at the top of src/lib.rs), tpl_cap NAME INDEX (capability type NAME holding INDEX as
# a u64), tpl_env CAP... (the full app environment over all caps), tpl_handler NAME CALLEE CAP...
# (`pub fn NAME`: an effect whose R is exactly CAP..., reading every cap and returning their sum,
# plus CALLEE's result when CALLEE is not `-`) and tpl_edge HANDLER... (`pub fn edge() -> u64`:
# builds the full env, runs every handler against it and returns the sum; it owns its runtime).
# Available to templates: CAP_NAMES (bash array), SCRIPT_DIR, REPO_ROOT, ID_EFFECT_PATH, GEN_CAPS,
# GEN_HANDLERS, GEN_TEMPLATE. templates/{plain,v06,next}.sh are worked examples.
#
# The handler plan is deterministic and identical for every template: handler i reads 1 + i % 5
# distinct capabilities (in a scattered, unsorted order), and every handler with i % 4 == 3 also
# calls handler i - 2 and therefore needs the union of both capability sets. Capability CapNN holds
# the value NN; a handler reads every capability in its R and returns their sum plus its callee's
# result (a callee's capabilities are a subset of the caller's R); `edge()` builds the
# full env and returns the sum over all handlers. The emitted crate carries a `#[cfg(test)]` that
# asserts that sum, so `cargo test` in it proves the template is faithful, while `cargo check`
# (what check-time.sh measures) never compiles that test.
#
# Default --out: target/check-time/gen/gen_caps_<template>_<CAPS>x<HANDLERS> (under the repo root).
# --dry-run writes nothing and prints the plan plus an excerpt of the crate instead.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
GEN_DIR="${REPO_ROOT}/target/check-time/gen"
export SCRIPT_DIR REPO_ROOT
export ID_EFFECT_PATH="${ID_EFFECT_PATH:-${REPO_ROOT}/crates/id_effect}"

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed -E 's/^# ?//'
}

die() {
    echo "gen-caps: $*" >&2
    exit 2
}

template_file() {
    case "$1" in
        plain | v06 | next) echo "${SCRIPT_DIR}/templates/$1.sh" ;;
        *) echo "$1" ;;
    esac
}

template_slug() {
    local base
    base="$(basename "$1")"
    base="${base%.sh}"
    case "$1" in
        plain | v06 | next) echo "$1" ;;
        *) echo "${base//[^A-Za-z0-9]/_}" ;;
    esac
}

# plan CAPS HANDLERS: fills CAP_NAMES, HANDLER_NAMES, HANDLER_CALLEE, HANDLER_CAPS (space-separated
# cap names), HANDLER_VALUE and EXPECTED_SUM.
plan() {
    local caps=$1 handlers=$2 cw=2 hw=3 i j k c start callee value own merged names
    local -A used
    ((caps >= 100)) && cw=${#caps}
    ((handlers > 1000)) && hw=${#handlers}
    CAP_NAMES=()
    HANDLER_NAMES=()
    HANDLER_CALLEE=()
    HANDLER_CAPS=()
    HANDLER_VALUE=()
    EXPECTED_SUM=0
    for ((c = 0; c < caps; c++)); do
        CAP_NAMES+=("$(printf 'Cap%0*d' "${cw}" "${c}")")
    done
    for ((i = 0; i < handlers; i++)); do
        HANDLER_NAMES+=("$(printf 'h%0*d' "${hw}" "${i}")")
        k=$((1 + i % 5))
        ((k > caps)) && k=${caps}
        start=$(((i * 7) % caps))
        used=()
        own=()
        for ((j = 0; j < k; j++)); do
            c=$(((start + j * 3) % caps))
            while [[ -n "${used[${c}]:-}" ]]; do
                c=$(((c + 1) % caps))
            done
            used[${c}]=1
            own+=("${c}")
        done
        callee=-
        merged=("${own[@]}")
        if ((i % 4 == 3 && i >= 2)); then
            callee=$((i - 2))
            for c in ${HANDLER_CAPS_IDX[${callee}]}; do
                [[ -n "${used[${c}]:-}" ]] || {
                    used[${c}]=1
                    merged+=("${c}")
                }
            done
        fi
        HANDLER_CAPS_IDX[i]="${merged[*]}"
        names=()
        value=0
        for c in "${merged[@]}"; do
            names+=("${CAP_NAMES[${c}]}")
            value=$((value + c))
        done
        if [[ "${callee}" != - ]]; then
            value=$((value + HANDLER_VALUE[callee]))
            HANDLER_CALLEE+=("${HANDLER_NAMES[${callee}]}")
        else
            HANDLER_CALLEE+=(-)
        fi
        HANDLER_CAPS+=("${names[*]}")
        HANDLER_VALUE+=("${value}")
        EXPECTED_SUM=$((EXPECTED_SUM + value))
    done
}

# emit TEMPLATE_FILE PKG OUT_DIR: write Cargo.toml and src/lib.rs.
emit() {
    local tpl=$1 pkg=$2 out=$3 i
    (
        # shellcheck source=/dev/null
        source "${tpl}"
        for fn in tpl_dependencies tpl_prelude tpl_cap tpl_env tpl_handler tpl_edge; do
            declare -F "${fn}" >/dev/null || die "template ${tpl} does not define ${fn}"
        done
        mkdir -p "${out}/src"
        {
            printf '# Generated by scripts/check-time/gen-caps.sh (%s caps x %s handlers, template %s). Do not edit.\n' \
                "${GEN_CAPS}" "${GEN_HANDLERS}" "${GEN_TEMPLATE}"
            printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2024"\npublish = false\n\n' "${pkg}"
            printf '[dependencies]\n'
            tpl_dependencies
            printf '\n# Standalone: never part of the root workspace.\n[workspace]\n'
        } >"${out}/Cargo.toml"
        {
            # shellcheck disable=SC2016 # the backticks are Markdown for rustdoc
            printf '//! Generated by scripts/check-time/gen-caps.sh: %s caps x %s handlers, template `%s`.\n' \
                "${GEN_CAPS}" "${GEN_HANDLERS}" "${GEN_TEMPLATE}"
            printf '//! Do not edit; regenerate instead.\n\n'
            tpl_prelude
            for ((i = 0; i < ${#CAP_NAMES[@]}; i++)); do
                tpl_cap "${CAP_NAMES[${i}]}" "${i}"
            done
            tpl_env "${CAP_NAMES[@]}"
            for ((i = 0; i < ${#HANDLER_NAMES[@]}; i++)); do
                # shellcheck disable=SC2086 # cap names are a word list by construction
                tpl_handler "${HANDLER_NAMES[${i}]}" "${HANDLER_CALLEE[${i}]}" ${HANDLER_CAPS[${i}]}
            done
            tpl_edge "${HANDLER_NAMES[@]}"
            printf '#[cfg(test)]\nmod gen_caps_tests {\n  #[test]\n  fn edge_sums_every_handler() {\n'
            printf '    assert_eq!(super::edge(), %s);\n  }\n}\n' "${EXPECTED_SUM}"
        } >"${out}/src/lib.rs"
    )
}

summary() {
    local i n hist=() composite=0
    for ((i = 0; i < ${#HANDLER_NAMES[@]}; i++)); do
        read -r -a caps_i <<<"${HANDLER_CAPS[${i}]}"
        n=${#caps_i[@]}
        hist[n]=$((${hist[n]:-0} + 1))
        [[ "${HANDLER_CALLEE[${i}]}" == - ]] || composite=$((composite + 1))
    done
    printf 'gen-caps: caps=%s handlers=%s template=%s composite=%s expected_sum=%s\n' \
        "${GEN_CAPS}" "${GEN_HANDLERS}" "${GEN_TEMPLATE}" "${composite}" "${EXPECTED_SUM}"
    printf 'gen-caps: caps per handler:'
    for n in "${!hist[@]}"; do
        printf ' %s=%s' "${n}" "${hist[${n}]}"
    done
    printf '\n'
}

self_test() {
    local tmp tpl_name=$1
    tmp="$(mktemp -d)"
    trap 'rm -rf "${tmp}"' RETURN
    "${BASH_SOURCE[0]}" 4 10 --template "${tpl_name}" --out "${tmp}/one" >/dev/null
    "${BASH_SOURCE[0]}" 4 10 --template "${tpl_name}" --out "${tmp}/two" >/dev/null
    if ! diff -r "${tmp}/one" "${tmp}/two" >/dev/null; then
        echo "gen-caps self-test FAILED: output is not deterministic" >&2
        return 1
    fi
    # 4x10 plan worked by hand: 0 + 5 + 3 + (6 + 5) + 6 + 3 + 3 + (4 + 3) + 6 + 6.
    grep -q 'assert_eq!(super::edge(), 50);' "${tmp}/one/src/lib.rs" || {
        echo "gen-caps self-test FAILED: the 4x10 plan changed (expected_sum != 50)" >&2
        return 1
    }
    if ! CARGO_TARGET_DIR="${tmp}/target" cargo test --quiet --manifest-path "${tmp}/one/Cargo.toml" >"${tmp}/test.log" 2>&1; then
        echo "gen-caps self-test FAILED: generated ${tpl_name} crate does not pass cargo test" >&2
        cat "${tmp}/test.log" >&2
        return 1
    fi
    echo "gen-caps self-test (${tpl_name}): ok"
}

caps=""
handlers=""
template=plain
out=""
dry_run=0
want_self_test=0
while (($# > 0)); do
    case "$1" in
        --self-test) want_self_test=1 ;;
        --dry-run) dry_run=1 ;;
        --template)
            template="${2:?--template needs a name or file}"
            shift
            ;;
        --out)
            out="${2:?--out needs a directory}"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*) die "unknown option $1" ;;
        *)
            if [[ -z "${caps}" ]]; then
                caps=$1
            elif [[ -z "${handlers}" ]]; then
                handlers=$1
            else
                die "unexpected argument $1"
            fi
            ;;
    esac
    shift
done

if ((want_self_test)); then
    self_test "${template}"
    exit
fi

[[ "${caps}" =~ ^[1-9][0-9]*$ && "${handlers}" =~ ^[1-9][0-9]*$ ]] || {
    usage >&2
    exit 2
}
tpl="$(template_file "${template}")"
[[ -f "${tpl}" ]] || die "template '${template}' not found at ${tpl}"
slug="$(template_slug "${template}")"
pkg="gen_caps_${slug}_${caps}x${handlers}"
export GEN_CAPS=${caps} GEN_HANDLERS=${handlers} GEN_TEMPLATE=${slug}

declare -a HANDLER_CAPS_IDX=()
plan "${caps}" "${handlers}"

if ((dry_run)); then
    tmp="$(mktemp -d)"
    trap 'rm -rf "${tmp}"' EXIT
    emit "${tpl}" "${pkg}" "${tmp}/${pkg}"
    summary
    printf 'gen-caps: dry run, would write %s (Cargo.toml: %s lines, src/lib.rs: %s lines)\n' \
        "${out:-${GEN_DIR}/${pkg}}" \
        "$(wc -l <"${tmp}/${pkg}/Cargo.toml")" "$(wc -l <"${tmp}/${pkg}/src/lib.rs")"
    printf -- '--- src/lib.rs (first 40 lines) ---\n'
    head -n 40 "${tmp}/${pkg}/src/lib.rs"
    exit 0
fi

out="${out:-${GEN_DIR}/${pkg}}"
rm -rf "${out}/src"
emit "${tpl}" "${pkg}" "${out}"
summary
echo "gen-caps: wrote ${out}"
