#!/usr/bin/env bash
# Measure the build loop of the template app (examples/cli-minimal unless CRATE_DIR is given):
# a cold `cargo build`, an incremental rebuild after a one-line change, and one run (boot) of the
# built binary. Each figure is the median of N runs (default 3).
#
# Usage:
#   scripts/compile-time.sh [CRATE_DIR] [--runs N] [--json] [--boot-args "ARGS"] [--keep]
#   scripts/compile-time.sh --self-test
#
# Requires: bash >= 4, cargo and rustc on PATH, GNU date (%N), awk, sed, sort, mktemp.
#
# Method: the crate is copied to .tmp/compile-time/ the way a user starts from the template:
# relative `path = "../…"` dependencies become absolute, `key.workspace = true` package keys take
# the root's [workspace.package] values, the copy gets its own [workspace] and the root
# Cargo.lock (same dependency versions). The repository sources are never edited. Every run then
#   1. cold:        deletes the copy's target dir and times `cargo build` (dev profile, the
#                   dependency sources already fetched, so no network time);
#   2. incremental: appends one line (`#[allow(dead_code)] const COMPILE_TIME_PROBE: u32 = <run>;`)
#                   to src/lib.rs (src/main.rs without a lib) and times `cargo build` again;
#   3. boot:        times one run of target/debug/<name> BOOT_ARGS (default `--token
#                   compile-time`) to its exit.
# RUSTC_WRAPPER, RUSTC_WORKSPACE_WRAPPER (and their CARGO_BUILD_ forms) and CARGO_INCREMENTAL are
# unset, so sccache is not used and the dev profile's own incremental default applies; the
# linker is the toolchain default. Run it on an idle machine; the load average is printed.
#
# Output: `compile-time: crate=<name> runs=<n> cold_s=<m> incremental_s=<m> boot_s=<m>
# cold_runs_s=<t1,..> incremental_runs_s=<..> boot_runs_s=<..> rustc=<v> wrapper=none`, or with
# --json one object {crate, cold_s, incremental_s, boot_s, runs, rustc, wrapper}. Progress and
# the load average go to stderr. --keep leaves the copy in .tmp/compile-time/ for inspection.
# Exit status: 0 ok, 1 FAIL (a build or the boot run failed, or the self-test failed), 2 misuse.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed -E 's/^# ?//'
}

die() {
    echo "compile-time: $*" >&2
    exit 2
}

fail() {
    echo "compile-time: FAIL: $*" >&2
    exit 1
}

now_ns() {
    date +%s%N
}

seconds_since() {
    awk -v ns=$(($(now_ns) - $1)) 'BEGIN { printf "%.3f", ns / 1e9 }'
}

median() {
    sort -g | awk '{ v[NR] = $1 } END { if (NR % 2) printf "%.3f", v[(NR + 1) / 2]; else printf "%.3f", (v[NR / 2] + v[NR / 2 + 1]) / 2 }'
}

# The value of KEY in the [workspace.package] table of MANIFEST (verbatim TOML, quotes kept).
ws_value() {
    awk -v key="$1" '
        /^\[/ { sec = $0; next }
        sec == "[workspace.package]" && $1 == key && $2 == "=" { sub(/^[^=]*= */, ""); print; exit }
    ' "$2"
}

# Copies the crate SRC to DEST as a standalone project (see Method).
prepare_copy() {
    local src="$1" dest="$2" root_manifest="${ROOT}/Cargo.toml" key value
    mkdir -p "${dest}"
    cp "${src}/Cargo.toml" "${dest}/Cargo.toml"
    cp -R "${src}/src" "${dest}/src"
    [[ ! -f "${src}/build.rs" ]] || cp "${src}/build.rs" "${dest}/build.rs"
    [[ ! -f "${ROOT}/Cargo.lock" ]] || cp "${ROOT}/Cargo.lock" "${dest}/Cargo.lock"
    sed -i -E "s#path = \"(\\.\\./[^\"]*)\"#path = \"${src}/\\1\"#g" "${dest}/Cargo.toml"
    while read -r key; do
        value="$(ws_value "${key}" "${root_manifest}")"
        [[ -n "${value}" ]] || die "${key}.workspace = true but the root has no [workspace.package] ${key}"
        sed -i -E "s#^${key}\\.workspace = true#${key} = ${value}#" "${dest}/Cargo.toml"
    done < <(sed -n -E 's/^([a-z-]+)\.workspace = true.*/\1/p' "${dest}/Cargo.toml")
    if grep -q 'workspace = true' "${dest}/Cargo.toml"; then
        die "unsupported workspace inheritance left in ${dest}/Cargo.toml"
    fi
    printf '\n[workspace]\n' >>"${dest}/Cargo.toml"
}

# The file the one-line change goes into: src/lib.rs, else src/main.rs.
edit_target() {
    if [[ -f "$1/src/lib.rs" ]]; then
        echo "$1/src/lib.rs"
    else
        echo "$1/src/main.rs"
    fi
}

one_line_change() {
    printf '#[allow(dead_code)] const COMPILE_TIME_PROBE: u32 = %d;\n' "$2" >>"$1"
}

emit() {
    # crate cold incr boot runs rustc json cold_runs incr_runs boot_runs
    if (($7)); then
        printf '{"crate": "%s", "cold_s": %s, "incremental_s": %s, "boot_s": %s, "runs": %s, "rustc": "%s", "wrapper": "none"}\n' \
            "$1" "$2" "$3" "$4" "$5" "$6"
    else
        printf 'compile-time: crate=%s runs=%s cold_s=%s incremental_s=%s boot_s=%s cold_runs_s=%s incremental_runs_s=%s boot_runs_s=%s rustc=%s wrapper=none\n' \
            "$1" "$5" "$2" "$3" "$4" "$8" "$9" "${10}" "$6"
    fi
}

self_test() {
    local tmp out status file before
    tmp="$(mktemp -d)"
    trap 'rm -rf "${tmp}"' RETURN
    if [[ "$(printf '3\n1\n2\n' | median)" != 2.000 || "$(printf '4\n1\n2\n3\n' | median)" != 2.500 ]]; then
        echo "compile-time self-test FAILED: median" >&2
        return 1
    fi
    out="$(emit cli_minimal 41.250 1.100 0.004 3 "rustc 1.0.0" 1 x y z)"
    if [[ ! "${out}" =~ ^\{\"crate\":\ \"cli_minimal\",\ \"cold_s\":\ 41.250,\ \"incremental_s\":\ 1.100,\ \"boot_s\":\ 0.004,\ \"runs\":\ 3,\ \"rustc\":\ \"rustc\ 1.0.0\",\ \"wrapper\":\ \"none\"\}$ ]]; then
        echo "compile-time self-test FAILED: unexpected json: ${out}" >&2
        return 1
    fi
    out="$(emit cli_minimal 41.250 1.100 0.004 3 "rustc 1.0.0" 0 1,2,3 4,5,6 7,8,9)"
    if [[ ! "${out}" =~ ^compile-time:\ crate=cli_minimal\ runs=3\ cold_s=41.250\ incremental_s=1.100\ boot_s=0.004\ cold_runs_s=1,2,3\  ]]; then
        echo "compile-time self-test FAILED: unexpected text: ${out}" >&2
        return 1
    fi
    prepare_copy "${ROOT}/examples/cli-minimal" "${tmp}/copy"
    if grep -q -E 'workspace = true|path = "\.\./' "${tmp}/copy/Cargo.toml" || ! grep -q '^\[workspace\]$' "${tmp}/copy/Cargo.toml"; then
        echo "compile-time self-test FAILED: the copy still depends on the workspace:" >&2
        cat "${tmp}/copy/Cargo.toml" >&2
        return 1
    fi
    while read -r dep; do
        [[ -f "${dep}/Cargo.toml" ]] || {
            echo "compile-time self-test FAILED: rewritten path ${dep} has no Cargo.toml" >&2
            return 1
        }
    done < <(sed -n -E 's/.*path = "(\/[^"]*)".*/\1/p' "${tmp}/copy/Cargo.toml")
    file="$(edit_target "${tmp}/copy")"
    before="$(wc -l <"${file}")"
    one_line_change "${file}" 7
    if [[ "$(wc -l <"${file}")" -ne $((before + 1)) ]] || ! tail -n 1 "${file}" | grep -q 'COMPILE_TIME_PROBE: u32 = 7;'; then
        echo "compile-time self-test FAILED: one-line change" >&2
        return 1
    fi
    for args in "--runs 0" "--runs x" "--bogus" "${tmp}/missing"; do
        status=0
        # shellcheck disable=SC2086 # split the argument list on purpose
        bash "${BASH_SOURCE[0]}" ${args} >/dev/null 2>&1 || status=$?
        if ((status != 2)); then
            echo "compile-time self-test FAILED: '${args}' must exit 2, got ${status}" >&2
            return 1
        fi
    done
    echo "compile-time self-test: ok"
}

runs=3
json=0
keep=0
crate=""
boot_args="--token compile-time"
while (($# > 0)); do
    case "$1" in
        --self-test)
            self_test || exit 1
            exit 0
            ;;
        --runs)
            (($# >= 2)) || die "--runs needs a number"
            runs="$2"
            shift
            ;;
        --boot-args)
            (($# >= 2)) || die "--boot-args needs a string"
            boot_args="$2"
            shift
            ;;
        --json) json=1 ;;
        --keep) keep=1 ;;
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
crate="${crate:-${ROOT}/examples/cli-minimal}"
[[ -f "${crate}/Cargo.toml" && -d "${crate}/src" ]] || die "no Cargo.toml and src/ in ${crate}"
crate="$(cd "${crate}" && pwd)"
name="$(sed -n 's/^name *= *"\(.*\)"/\1/p' "${crate}/Cargo.toml" | head -n 1)"
[[ -n "${name}" ]] || die "no package name in ${crate}/Cargo.toml"

unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL

work="${ROOT}/.tmp/compile-time/${name}-$$"
cleanup() {
    if ((keep)); then
        echo "compile-time: kept ${work}" >&2
    else
        rm -rf "${work}"
    fi
}
trap cleanup EXIT
prepare_copy "${crate}" "${work}"
manifest="${work}/Cargo.toml"
export CARGO_TARGET_DIR="${work}/target"
log="${work}/cargo.log"
file="$(edit_target "${work}")"
cp "${file}" "${work}/pristine.rs"
read -r -a boot_argv <<<"${boot_args}"

cargo_build() {
    cargo build --quiet --manifest-path "${manifest}" >"${log}" 2>&1 || {
        cat "${log}" >&2
        fail "cargo build failed for ${name} ($1)"
    }
}

echo "compile-time: ${name}: fetching dependencies (untimed); load average: $(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || echo n/a)" >&2
cargo fetch --quiet --manifest-path "${manifest}" >"${log}" 2>&1 || {
    cat "${log}" >&2
    fail "cargo fetch failed for ${name}"
}
cold=()
incr=()
boot=()
for ((i = 1; i <= runs; i++)); do
    rm -rf "${CARGO_TARGET_DIR}"
    cp "${work}/pristine.rs" "${file}"
    start=$(now_ns)
    cargo_build "cold, run ${i}"
    cold+=("$(seconds_since "${start}")")
    one_line_change "${file}" "${i}"
    start=$(now_ns)
    cargo_build "incremental, run ${i}"
    incr+=("$(seconds_since "${start}")")
    start=$(now_ns)
    "${CARGO_TARGET_DIR}/debug/${name}" "${boot_argv[@]}" >"${work}/boot.log" 2>&1 || {
        cat "${work}/boot.log" >&2
        fail "the boot run of ${name} failed (run ${i})"
    }
    boot+=("$(seconds_since "${start}")")
    echo "compile-time: run ${i}/${runs}: cold_s=${cold[-1]} incremental_s=${incr[-1]} boot_s=${boot[-1]} load=$(cut -d' ' -f1 /proc/loadavg 2>/dev/null || echo n/a)" >&2
done

joined() {
    local s
    s="$(printf '%s,' "$@")"
    echo "${s%,}"
}
emit "${name}" \
    "$(printf '%s\n' "${cold[@]}" | median)" \
    "$(printf '%s\n' "${incr[@]}" | median)" \
    "$(printf '%s\n' "${boot[@]}" | median)" \
    "${runs}" "$(rustc -V)" "${json}" \
    "$(joined "${cold[@]}")" "$(joined "${incr[@]}")" "$(joined "${boot[@]}")"
