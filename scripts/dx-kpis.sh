#!/usr/bin/env bash
# Measure the developer-experience KPIs and record them in docs/benchmarks/dx-kpis-<release>.json
# (release DX_KPIS_RELEASE, default 0.9).
#
#   K1  time to a running service: wall time from `id-effect new svc --template service
#       --local-workspace <repo>` to `GET /health` answering 200, with a cold build in a fresh
#       target dir (budget: K1_BUDGET_S, default 180 s).
#   K3  feature LOC: the added non-blank lines of docs/benchmarks/k3/invoice.patch, the "invoice"
#       resource (model, migration, routes, policy, test) added to examples/reference/ledger,
#       per category. Measured, no target (PLAN-OF-PLANS §8: -40 % at a later release). The
#       "job" of §8 is excluded until the jobs battery exists (0.10).
#   K4  diagnostics: the cases of crates/id_effect/tests/ui/gallery whose first error states the
#       fix, as recorded in the "Fix in the error" column of docs/diagnostics.md (at least K4_MIN,
#       default 17, owner-accepted for 0.8).
#   K5  compiled book: rust fences of crates/id_effect/book/src that a doctest compiles, over all
#       rust fences (at least K5_MIN_PERCENT, default 80).
#
# Usage:
#   scripts/dx-kpis.sh [--json] [--out FILE] [--skip-k1] [--k3-build] [--keep]
#   scripts/dx-kpis.sh --self-test
#
#     --json      print the result as one JSON object (default: one line per KPI)
#     --out FILE  where to write the JSON (default docs/benchmarks/dx-kpis-<release>.json; written
#                 only when K1 was measured or --out is given)
#     --skip-k1   only the static KPIs K3, K4 and K5 (seconds instead of minutes); k1 is null
#     --k3-build  also build the patched Ledger and run its invoice tests (minutes; the tests
#                 need Docker and print SKIP without it, unless ID_EFFECT_REQUIRE_DOCKER=1)
#     --keep      keep the K1 and K3 work directories
#
# Environment:
#   DX_KPIS_DIR  where the K1 project, the K3 copy and their target dirs go (default: a new
#                `mktemp -d`); must be outside the repository. The `id-effect` binary is built
#                first, untimed, with the caller's CARGO_TARGET_DIR (default: <repo>/target).
#   K3_TARGET_DIR  the target dir of the --k3-build build (default: <work>/k3-target).
#
# Method:
#   K1  RUSTC_WRAPPER, RUSTC_WORKSPACE_WRAPPER (and their CARGO_BUILD_ forms) and
#       CARGO_INCREMENTAL are unset, so sccache does not make the build warm. The timed span is:
#       `id-effect new`, copying the root Cargo.lock (the workspace's dependency versions),
#       `cargo build` into an empty target dir, starting the binary with HOST=127.0.0.1 and a free
#       PORT, and polling /health every 0.1 s until it answers 200 (60 s limit after the build).
#       Dependency sources come from the local cargo registry cache when they are there, so
#       download time is not part of the figure on a machine that has built the workspace.
#   K3  the patch is a unified diff with repository-relative paths (a/examples/reference/ledger/...)
#       and must apply to HEAD (`git apply --cached --check` on a scratch index read from HEAD;
#       the working tree is not touched). A counted line is an added line (`+`, not the `+++`
#       header) with a non-blank remainder; a changed line counts as added. Its category comes
#       from its path under examples/reference/ledger:
#         migrations/**      migration
#         tests/**           test (the new test file and the existing tests the change updates)
#         src/policy.rs      policy (permissions, named policies, Owned / Tenanted impls)
#         src/app.rs         routes (the router's merge)
#         src/lib.rs         model (the module declaration)
#         other src/*.rs     the resource module: model (entity, repository, input schemas,
#                            lookups) up to its first added private top-level `fn` (the first
#                            handler), routes from that line on (handlers and `routes()`)
#         anything else      other (counted in the total, printed when not 0)
#       --k3-build exports HEAD (`git archive`) into <work>/k3, applies the patch there, builds
#       `cargo build -p ledger --tests` and runs `cargo test -p ledger --test invoices`.
#   K4  a gallery row starts with its number and the name in backticks and ends with the
#       "Fix in the error" cell, which starts with `yes` or `no`. Every row needs <name>.rs and
#       <name>.stderr in the gallery, and every gallery file needs a row (FAIL otherwise).
#   K5  a rust fence is a backtick or tilde fence whose info string is empty, starts with `rust`,
#       or is a rustdoc attribute (`no_run`, `ignore`, ...). It is compiled when its page is
#       included as a doctest (an include_str! of ".../book/src/<page>.md" under
#       examples/book-snippets/src or crates/id_effect/src) and the fence is not `ignore`;
#       `compile_fail` and `no_run` count as compiled. `cargo test --doc -p book_snippets` is the
#       check that they do compile.
#
# Output: `dx-kpis: K1 <s>s (budget <b>s ...) ok|FAIL`, `dx-kpis: K3 <n> lines ... measured`,
# `dx-kpis: K4 <n>/<m> ...`, `dx-kpis: K5 <c>/<t> rust fences compiled = <p>% ...`, or with --json
# the object written to --out. Progress and the machine's load go to stderr. Exit status: 0 every
# KPI meets its target (K3 has none: it fails only when the patch does not apply or, with
# --k3-build, does not build or pass), 1 FAIL (a target missed, a measurement or the self-test
# failed), 2 misuse. The written file is formatted with biome when it is on PATH, like every JSON
# file in the repository.
# Requires: bash >= 4.4, cargo and rustc on PATH, curl, git, GNU coreutils (date %N, realpath -m,
# sort -z, mktemp, nproc), awk, sed, grep, find, tar.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
RELEASE="${DX_KPIS_RELEASE:-0.9}"
K3_PATCH="${ROOT}/docs/benchmarks/k3/invoice.patch"
K3_APP="examples/reference/ledger"
K1_BUDGET_S="${K1_BUDGET_S:-180}"
K4_MIN="${K4_MIN:-17}"
K5_MIN_PERCENT="${K5_MIN_PERCENT:-80}"
# One and three backticks, spelled without them so formatters do not read a command substitution.
BT="$(printf '\140')"
FENCE="${BT}${BT}${BT}"

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed -E 's/^# ?//'
}

die() {
    echo "dx-kpis: $*" >&2
    exit 2
}

fail() {
    echo "dx-kpis: FAIL: $*" >&2
    exit 1
}

now_ns() {
    date +%s%N
}

seconds_since() {
    awk -v ns=$(($(now_ns) - $1)) 'BEGIN { printf "%.2f", ns / 1e9 }'
}

json_string() {
    local s="$1"
    s="${s//\\/\\\\}"
    s="${s//\"/\\\"}"
    printf '"%s"' "${s}"
}

json_bool() {
    if (($1)); then echo true; else echo false; fi
}

ok_word() {
    if (($1)); then echo ok; else echo FAIL; fi
}

# 1 when the decimal A is at most B, else 0.
at_most() {
    awk -v a="$1" -v b="$2" 'BEGIN { print (a <= b ? 1 : 0) }'
}

# 1 when COMPILED / FENCES is at least MIN percent, else 0.
percent_ok() {
    awk -v c="$1" -v t="$2" -v m="$3" 'BEGIN { print (t && 100 * c >= m * t ? 1 : 0) }'
}

percent() {
    awk -v c="$1" -v t="$2" 'BEGIN { printf "%.1f", (t ? 100 * c / t : 0) }'
}

# --- K3 -------------------------------------------------------------------------------------

# Prints `<total> <model> <migration> <routes> <policy> <test> <other> <files>` for the unified
# diff PATCH: its added non-blank lines by category (rules in the header) and the files it touches.
k3_count() {
    awk -v app="${K3_APP}/" '
        function category(path, line) {
            if (index(path, app) != 1) return "other"
            path = substr(path, length(app) + 1)
            if (path ~ /^migrations\//) return "migration"
            if (path ~ /^tests\//) return "test"
            if (path == "src/policy.rs") return "policy"
            if (path == "src/app.rs") return "routes"
            if (path == "src/lib.rs") return "model"
            if (path ~ /^src\/[^\/]+\.rs$/) {
                if (line ~ /^fn /) handlers[path] = 1
                return (path in handlers ? "routes" : "model")
            }
            return "other"
        }
        /^diff --git / { header = 1; files++; next }
        header && /^\+\+\+ / { file = $2; sub(/^b\//, "", file); next }
        /^@@ / { header = 0; next }
        header { next }
        /^\+/ {
            line = substr($0, 2)
            if (line !~ /[^ \t\r]/) next
            n[category(file, line)]++
            total++
        }
        END {
            printf "%d %d %d %d %d %d %d %d\n", total, n["model"], n["migration"], n["routes"],
                n["policy"], n["test"], n["other"], files
        }' "$1"
}

# Succeeds when PATCH applies to the tree of HEAD: checked against a scratch index read from
# HEAD, so neither the working tree nor the real index matters.
k3_applies() {
    local patch dir status=0
    patch="$(realpath "$1")"
    dir="$(mktemp -d)"
    GIT_INDEX_FILE="${dir}/index" git -C "${ROOT}" read-tree HEAD &&
    GIT_INDEX_FILE="${dir}/index" git -C "${ROOT}" apply --cached --check "${patch}" || status=$?
    rm -rf "${dir}"
    return "${status}"
}

# Exports HEAD into DIR/k3, applies PATCH there, builds the Ledger with its tests and runs the
# invoice tests; prints `<build_s> <test_s> passed|skipped`.
k3_build() {
    local dir="$1" patch src="$1/k3" target="${K3_TARGET_DIR:-$1/k3-target}" log="$1/k3.log"
    local t build_s test_s outcome=passed
    patch="$(realpath "$2")"
    rm -rf "${src}"
    mkdir -p "${src}"
    git -C "${ROOT}" archive HEAD | tar -x -C "${src}" || fail "K3: git archive HEAD"
    # The export is no repository: stop git from finding one above it.
    GIT_CEILING_DIRECTORIES="${dir}" git -C "${src}" apply "${patch}" ||
    fail "K3: the patch does not apply to the export of HEAD"
    echo "dx-kpis: K3: building the patched Ledger in ${src}" >&2
    t="$(now_ns)"
    if ! (cd "${src}" && CARGO_TARGET_DIR="${target}" cargo build --quiet -p ledger --tests) >"${log}" 2>&1; then
        tail -n 40 "${log}" >&2
        fail "K3: the patched Ledger does not build"
    fi
    build_s="$(seconds_since "${t}")"
    t="$(now_ns)"
    if ! (cd "${src}" && CARGO_TARGET_DIR="${target}" cargo test --quiet -p ledger --test invoices -- --nocapture) >"${log}" 2>&1; then
        tail -n 40 "${log}" >&2
        fail "K3: the invoice tests of the patched Ledger fail"
    fi
    test_s="$(seconds_since "${t}")"
    ! grep -q SKIP "${log}" || outcome=skipped
    echo "${build_s} ${test_s} ${outcome}"
}

# --- K4 -------------------------------------------------------------------------------------

# Prints `<fixed> <rows> <names without the fix, comma-separated, or ->` for the diagnostics
# table in DOC and the gallery directory GALLERY; returns 1 (the reason on stderr) when they
# disagree.
k4_count() {
    local doc="$1" gallery="$2" rows entry missing="" extra=""
    rows="$(awk -F'|' '
        BEGIN { name = "^ *\140[a-z0-9_]+\140 *$" }
        $2 ~ /^ *[0-9]+ *$/ && $3 ~ name {
            row = $3
            gsub(/[^a-z0-9_]/, "", row)
            fix = $(NF - 1)
            sub(/^ +/, "", fix)
            print row, (fix ~ /^yes/ ? "yes" : "no")
        }' "${doc}")"
    if [[ -z "${rows}" ]]; then
        echo "no gallery rows in ${doc}" >&2
        return 1
    fi
    while read -r entry _; do
        [[ -f "${gallery}/${entry}.rs" && -f "${gallery}/${entry}.stderr" ]] || missing+=" ${entry}"
    done <<<"${rows}"
    for entry in "${gallery}"/*.rs; do
        entry="$(basename "${entry}" .rs)"
        grep -q "^${entry} " <<<"${rows}" || extra+=" ${entry}"
    done
    if [[ -n "${missing}${extra}" ]]; then
        [[ -z "${missing}" ]] || echo "rows without both .rs and .stderr in the gallery:${missing}" >&2
        [[ -z "${extra}" ]] || echo "gallery files without a row in ${doc}:${extra}" >&2
        return 1
    fi
    awk '
        { n++ }
        $2 == "yes" { y++ }
        $2 == "no" { no = no (no == "" ? "" : ",") $1 }
        END { printf "%d %d %s\n", y, n, (no == "" ? "-" : no) }' <<<"${rows}"
}

# --- K5 -------------------------------------------------------------------------------------

# The book pages (paths relative to book/src) included as doctests by the files under DIRS.
k5_included() {
    grep -rhoE 'include_str!\("[^"]*book/src/[^"]+\.md"\)' "$@" 2>/dev/null |
    sed -E 's#.*book/src/([^"]+)".*#\1#' | sort -u
}

# Prints `<compiled> <rust fences> <ignored> <not included>` for the pages under BOOK_SRC, given
# the included pages in the file INCLUDED (one path relative to BOOK_SRC per line).
k5_count() {
    local book_src="$1" included="$2" pages
    mapfile -d '' pages < <(find "${book_src}" -name '*.md' -print0 | sort -z)
    awk -v src="${book_src}/" -v inc="${included}" '
        BEGIN {
            while ((getline line < inc) > 0) included[line] = 1
            split("ignore no_run should_panic compile_fail test_harness standalone_crate", a, " ")
            for (i in a) attr[a[i]] = 1
            bt = sprintf("%c", 96)
            opening = "^[ \t]*(" bt bt bt "+|~~~+)"
        }
        FNR == 1 {
            open = 0
            page = substr(FILENAME, length(src) + 1)
        }
        open {
            t = $0
            gsub(/^[ \t]+|[ \t]+$/, "", t)
            closing = "^" (fch == bt ? bt "+" : "~+") "$"
            if (length(t) >= flen && t ~ closing) open = 0
            next
        }
        match($0, opening) {
            fence = substr($0, RSTART, RLENGTH)
            sub(/^[ \t]+/, "", fence)
            info = tolower(substr($0, RSTART + RLENGTH))
            fch = substr(fence, 1, 1)
            if (fch == bt && index(info, bt)) next
            open = 1
            flen = length(fence)
            gsub(/^[ \t{.]+|[ \t}]+$/, "", info)
            gsub(/[ \t]+/, ",", info)
            n = split(info, tok, ",")
            first = (n ? tok[1] : "")
            if (!(first == "" || first ~ /^rust/ || first in attr || first ~ /^edition[0-9]+$/)) next
            ignored = 0
            for (i = 1; i <= n; i++) if (tok[i] == "ignore" || tok[i] ~ /^ignore-/) ignored = 1
            total++
            if (ignored) ign++
            else if (page in included) compiled++
            else notincl++
        }
        END { printf "%d %d %d %d\n", compiled, total, ign, notincl }' "${pages[@]}"
}

# --- K1 -------------------------------------------------------------------------------------

# The first port from 18080 that nothing accepts connections on.
free_port() {
    local port
    for ((port = 18080; port < 19080; port++)); do
        if ! (exec 3<>"/dev/tcp/127.0.0.1/${port}") 2>/dev/null; then
            echo "${port}"
            return 0
        fi
    done
    return 1
}

# Measures K1 in DIR with the id-effect binary BIN; prints `<total_s> <new_s> <build_s> <boot_s>`.
k1_measure() {
    local dir="$1" bin="$2" project="$1/svc" target="$1/target" log="$1/build.log"
    local port t0 t new_s build_s boot_s total_s pid code="" deadline
    rm -rf "${project}" "${target}"
    port="$(free_port)" || fail "K1: no free port in 18080-19079"
    unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_RUSTC_WRAPPER CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_INCREMENTAL
    echo "dx-kpis: K1: cold build of a new service in ${dir}; load average: $(cut -d' ' -f1-3 /proc/loadavg)" >&2
    t0="$(now_ns)"
    "${bin}" new svc --template service --dest "${project}" --local-workspace "${ROOT}" >/dev/null ||
    fail "K1: id-effect new failed"
    [[ ! -f "${ROOT}/Cargo.lock" ]] || cp "${ROOT}/Cargo.lock" "${project}/Cargo.lock"
    new_s="$(seconds_since "${t0}")"
    t="$(now_ns)"
    if ! CARGO_TARGET_DIR="${target}" cargo build --quiet --manifest-path "${project}/Cargo.toml" --bin svc >"${log}" 2>&1; then
        tail -n 40 "${log}" >&2
        fail "K1: cargo build failed"
    fi
    build_s="$(seconds_since "${t}")"
    t="$(now_ns)"
    HOST=127.0.0.1 PORT="${port}" "${target}/debug/svc" >"${dir}/svc.log" 2>&1 &
    pid=$!
    deadline=$(($(now_ns) + 60 * 1000000000))
    while (($(now_ns) < deadline)); do
        code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/health" || true)"
        [[ "${code}" != 200 ]] || break
        kill -0 "${pid}" 2>/dev/null || break
        sleep 0.1
    done
    boot_s="$(seconds_since "${t}")"
    total_s="$(seconds_since "${t0}")"
    kill "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
    if [[ "${code}" != 200 ]]; then
        cat "${dir}/svc.log" >&2
        fail "K1: GET /health answered '${code}', not 200"
    fi
    echo "${total_s} ${new_s} ${build_s} ${boot_s}"
}

# --- output ---------------------------------------------------------------------------------

# The machine, with the load average (1, 5, 15 min) at the START and at the end of the run.
machine_json() {
    local start="$1" cpu mem load
    cpu="$(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo | head -n 1)"
    mem="$(awk '/^MemTotal:/ { printf "%.0f", $2 / 1048576 }' /proc/meminfo)"
    load="$(cut -d' ' -f1-3 /proc/loadavg)"
    printf '{"cpu": %s, "cores": %s, "mem_gib": %s, "os": %s, "load_avg_start": %s, "load_avg_end": %s}' \
        "$(json_string "${cpu:-unknown}")" "$(nproc)" "${mem:-null}" "$(json_string "$(uname -sr)")" \
        "$(json_string "${start}")" "$(json_string "${load}")"
}

# The JSON list of the comma-separated NAMES (`-` for none).
json_list() {
    [[ "$1" == - ]] || sed -E 's/([^,]+)/"\1"/g; s/,/, /g' <<<"$1"
}

# Prints the JSON object for K1 ("total new build boot", or empty when skipped), K4 ("fixed
# rows without"), K5 ("compiled fences ignored notincl"), the machine JSON, rustc, revision, K3
# (the k3_count line) and the K3 build ("build_s test_s outcome", or empty without --k3-build).
emit_json() {
    local k1="$1" k4="$2" k5="$3" machine="$4" rustc="$5" rev="$6" k3="$7" k3b="$8" k1_json="null"
    local total new build boot fixed rows without compiled fences ignored notincl
    local lines model migration routes policy tests other files k3b_json="null" tested outcome
    read -r lines model migration routes policy tests other files <<<"${k3}"
    if [[ -n "${k3b}" ]]; then
        read -r build tested outcome <<<"${k3b}"
        k3b_json="{\"build_s\": ${build}, \"test_s\": ${tested}, \"invoice_tests\": \"${outcome}\"}"
    fi
    if [[ -n "${k1}" ]]; then
        read -r total new build boot <<<"${k1}"
        k1_json="{\"seconds\": ${total}, \"budget_s\": ${K1_BUDGET_S}, \"pass\": $(json_bool "$(at_most "${total}" "${K1_BUDGET_S}")"), \"new_s\": ${new}, \"build_s\": ${build}, \"boot_to_health_s\": ${boot}, \"template\": \"service\", \"target_dir\": \"fresh\", \"wrapper\": \"none\"}"
    fi
    read -r fixed rows without <<<"${k4}"
    read -r compiled fences ignored notincl <<<"${k5}"
    echo "{"
    echo "  \"schema\": \"id_effect dx-kpis v1\","
    echo "  \"method\": \"scripts/dx-kpis.sh (header)\","
    echo "  \"release\": $(json_string "${RELEASE}"),"
    echo "  \"measured_at\": $(json_string "$(date -u +%Y-%m-%dT%H:%M:%SZ)"),"
    echo "  \"revision\": $(json_string "${rev}"),"
    echo "  \"rustc\": $(json_string "${rustc}"),"
    echo "  \"machine\": ${machine},"
    echo "  \"k1\": ${k1_json},"
    echo "  \"k3\": {\"lines\": ${lines}, \"by_category\": {\"model\": ${model}, \"migration\": ${migration}, \"routes\": ${routes}, \"policy\": ${policy}, \"test\": ${tests}, \"other\": ${other}}, \"files\": ${files}, \"patch\": \"docs/benchmarks/k3/invoice.patch\", \"applies_to_head\": true, \"excluded\": {\"job\": \"no jobs battery before 0.10\"}, \"build\": ${k3b_json}, \"target\": \"none at 0.9 (measured); PLAN-OF-PLANS §8: -40 % vs 0.9 at a later release\"},"
    echo "  \"k4\": {\"fixed\": ${fixed}, \"cases\": ${rows}, \"min\": ${K4_MIN}, \"pass\": $(json_bool $((fixed >= K4_MIN))), \"without_fix\": [$(json_list "${without}")]},"
    echo "  \"k5\": {\"compiled\": ${compiled}, \"rust_fences\": ${fences}, \"ignored\": ${ignored}, \"not_included\": ${notincl}, \"percent\": $(percent "${compiled}" "${fences}"), \"min_percent\": ${K5_MIN_PERCENT}, \"pass\": $(json_bool "$(percent_ok "${compiled}" "${fences}" "${K5_MIN_PERCENT}")")}"
    echo "}"
}

# --- self-test ------------------------------------------------------------------------------

st_fail() {
    echo "dx-kpis self-test FAILED: $*" >&2
    return 1
}

self_test() {
    local tmp out status args entry want machine
    tmp="$(mktemp -d)"
    trap 'rm -rf "${tmp}"' RETURN

    # K5: fence kinds, a fence nested in a longer one, indentation, included or not.
    mkdir -p "${tmp}/book/src/part1" "${tmp}/harness"
    printf '%s\n' "# A" "${FENCE}rust" "let a = 1;" "${FENCE}" "${FENCE}rust,no_run" "fn main() {}" "${FENCE}" \
        "${FENCE}rust,ignore" "nope" "${FENCE}" "${FENCE}text" "not rust" "${FENCE}" \
        "${FENCE}${BT}markdown" "${FENCE}rust" "inside a longer fence: not a fence" "${FENCE}" "${FENCE}${BT}" \
        "  ${FENCE}" "  bare and indented: rust" "  ${FENCE}" "~~~rust,compile_fail" "let x: u8 = 1;" "~~~" \
        "${FENCE}sh" "echo hi" "${FENCE}" >"${tmp}/book/src/part1/a.md"
    printf '%s\n' "${FENCE}rust" "not included" "${FENCE}" "${FENCE}rust,ignore" "ignored" "${FENCE}" >"${tmp}/book/src/b.md"
    printf '#[doc = include_str!("../../book/src/part1/a.md")]\nstruct A;\n' >"${tmp}/harness/lib.rs"
    k5_included "${tmp}/harness" >"${tmp}/included"
    [[ "$(cat "${tmp}/included")" == part1/a.md ]] || st_fail "k5_included: $(cat "${tmp}/included")" || return 1
    out="$(k5_count "${tmp}/book/src" "${tmp}/included")"
    [[ "${out}" == "4 7 2 1" ]] || st_fail "k5_count: want '4 7 2 1', got '${out}'" || return 1

    # K4: rows, the fix column (escaped pipes in a cell), consistency with the gallery.
    mkdir -p "${tmp}/gallery"
    printf '%s\n' "| # | Name | Mistake | First error | Fix | Fix in the error |" \
        "|---|------|---------|-------------|-----|------------------|" \
        "| 1 | ${BT}alpha${BT} | a | E0277 | fix \\| it | yes (${BT}Selector${BT} note) |" \
        "| 2 | ${BT}beta_2${BT} | b | E0283 | fix | no (rustc) |" \
        "| 3 | ${BT}gamma${BT} | c | E0277 | fix | yes (derive message) |" >"${tmp}/diag.md"
    for entry in alpha beta_2 gamma; do
        : >"${tmp}/gallery/${entry}.rs"
        : >"${tmp}/gallery/${entry}.stderr"
    done
    out="$(k4_count "${tmp}/diag.md" "${tmp}/gallery")"
    [[ "${out}" == "2 3 beta_2" ]] || st_fail "k4_count: want '2 3 beta_2', got '${out}'" || return 1
    : >"${tmp}/gallery/delta.rs"
    if k4_count "${tmp}/diag.md" "${tmp}/gallery" >/dev/null 2>&1; then
        st_fail "k4_count accepted a gallery file without a row" || return 1
    fi
    rm "${tmp}/gallery/delta.rs" "${tmp}/gallery/gamma.stderr"
    if k4_count "${tmp}/diag.md" "${tmp}/gallery" >/dev/null 2>&1; then
        st_fail "k4_count accepted a row without a .stderr" || return 1
    fi

    # K3: categories, blank and whitespace-only lines, a `+++` content line, a changed line.
    printf '%s\n' "diff --git a/${K3_APP}/src/widgets.rs b/${K3_APP}/src/widgets.rs" "new file mode 100644" \
        "--- /dev/null" "+++ b/${K3_APP}/src/widgets.rs" "@@ -0,0 +1,6 @@" "+//! Widgets." "+" \
        "+pub struct Widget;" "+fn list() {}" "+  " "+pub fn routes() {}" \
        "diff --git a/${K3_APP}/migrations/0009_w.up.sql b/${K3_APP}/migrations/0009_w.up.sql" \
        "--- /dev/null" "+++ b/${K3_APP}/migrations/0009_w.up.sql" "@@ -0,0 +1,2 @@" "+CREATE TABLE w ();" \
        "+++ content, not a header" "diff --git a/${K3_APP}/src/policy.rs b/${K3_APP}/src/policy.rs" \
        "--- a/${K3_APP}/src/policy.rs" "+++ b/${K3_APP}/src/policy.rs" "@@ -1,2 +1,2 @@" " keep" "-old" "+new" \
        "diff --git a/${K3_APP}/tests/w.rs b/${K3_APP}/tests/w.rs" "--- /dev/null" "+++ b/${K3_APP}/tests/w.rs" \
        "@@ -0,0 +1 @@" "+#[test]" "diff --git a/${K3_APP}/src/app.rs b/${K3_APP}/src/app.rs" \
        "--- a/${K3_APP}/src/app.rs" "+++ b/${K3_APP}/src/app.rs" "@@ -1 +1,2 @@" " keep" "+.merge(w)" \
        "diff --git a/README.md b/README.md" "--- a/README.md" "+++ b/README.md" "@@ -1 +1,2 @@" " keep" "+doc" >"${tmp}/k3.patch"
    out="$(k3_count "${tmp}/k3.patch")"
    [[ "${out}" == "10 2 2 3 1 1 1 6" ]] || st_fail "k3_count: want '10 2 2 3 1 1 1 6', got '${out}'" || return 1
    if k3_applies "${tmp}/k3.patch" 2>/dev/null; then
        st_fail "k3_applies accepted a patch that does not apply to HEAD" || return 1
    fi

    # The real inputs parse (no target is checked here).
    k3_applies "${K3_PATCH}" || st_fail "${K3_PATCH} does not apply to HEAD" || return 1
    out="$(k3_count "${K3_PATCH}")"
    [[ "${out}" =~ ^[1-9][0-9]*\  ]] || st_fail "k3_count on ${K3_PATCH}: ${out}" || return 1
    k4_count "${ROOT}/docs/diagnostics.md" "${ROOT}/crates/id_effect/tests/ui/gallery" >/dev/null ||
    st_fail "k4_count on docs/diagnostics.md" || return 1
    k5_included "${ROOT}/examples/book-snippets/src" "${ROOT}/crates/id_effect/src" >"${tmp}/real-included"
    [[ -s "${tmp}/real-included" ]] || st_fail "no included book pages found" || return 1

    # The JSON, with and without K1, passing and failing.
    machine='{"cpu": "x"}'
    out="$(emit_json "95.5 0.1 90.2 5.2" "17 20 a,b,c" "350 424 20 54" "${machine}" "rustc 1.0" "abc" "10 2 2 3 1 1 1 6" "61.5 3.2 passed")"
    for want in '"k1": {"seconds": 95.5, "budget_s": 180, "pass": true, "new_s": 0.1, "build_s": 90.2,' \
        '"k3": {"lines": 10, "by_category": {"model": 2, "migration": 2, "routes": 3, "policy": 1, "test": 1, "other": 1}, "files": 6,' \
        '"build": {"build_s": 61.5, "test_s": 3.2, "invoice_tests": "passed"},' \
        '"k4": {"fixed": 17, "cases": 20, "min": 17, "pass": true, "without_fix": ["a", "b", "c"]},' \
        '"k5": {"compiled": 350, "rust_fences": 424, "ignored": 20, "not_included": 54, "percent": 82.5, "min_percent": 80, "pass": true}' \
        '"machine": {"cpu": "x"},' '"revision": "abc",'; do
        [[ "${out}" == *"${want}"* ]] || st_fail "the JSON lacks ${want}: ${out}" || return 1
    done
    out="$(emit_json "" "16 20 -" "1 10 0 9" "${machine}" "rustc 1.0" "abc" "1 1 0 0 0 0 0 1" "")"
    for want in '"k1": null,' '"build": null,' '"pass": false, "without_fix": []},' '"percent": 10.0, "min_percent": 80, "pass": false}'; do
        [[ "${out}" == *"${want}"* ]] || st_fail "the JSON lacks ${want}: ${out}" || return 1
    done
    out="$(emit_json "200.0 0.1 195.0 4.9" "17 20 -" "9 10 1 0" "${machine}" r x "1 1 0 0 0 0 0 1" "")"
    [[ "${out}" == *'"seconds": 200.0, "budget_s": 180, "pass": false,'* ]] || st_fail "K1 over budget passes: ${out}" || return 1
    [[ "$(json_string 'a "b" \c')" == '"a \"b\" \\c"' ]] || st_fail "json_string" || return 1
    [[ "$(free_port)" =~ ^[0-9]+$ ]] || st_fail "free_port" || return 1

    # Misuse exits 2, before anything is built.
    for args in "--bogus" "extra" "--out"; do
        status=0
        # shellcheck disable=SC2086 # split the argument list on purpose
        bash "${BASH_SOURCE[0]}" ${args} >/dev/null 2>&1 || status=$?
        ((status == 2)) || st_fail "'${args}' must exit 2, got ${status}" || return 1
    done
    status=0
    DX_KPIS_DIR="${ROOT}/.tmp/dx-kpis-inside" bash "${BASH_SOURCE[0]}" >/dev/null 2>&1 || status=$?
    ((status == 2)) || st_fail "DX_KPIS_DIR inside the repository must exit 2, got ${status}" || return 1
    [[ ! -e "${ROOT}/.tmp/dx-kpis-inside" ]] || st_fail "a refused DX_KPIS_DIR was created" || return 1
    status=0
    K4_MIN=x bash "${BASH_SOURCE[0]}" --skip-k1 >/dev/null 2>&1 || status=$?
    ((status == 2)) || st_fail "K4_MIN=x must exit 2, got ${status}" || return 1
    echo "dx-kpis self-test: ok"
}

# --- main -----------------------------------------------------------------------------------

json=0
keep=0
skip_k1=0
build_k3=0
out=""
while (($# > 0)); do
    case "$1" in
        --self-test)
            self_test || exit 1
            exit 0
            ;;
        --json) json=1 ;;
        --keep) keep=1 ;;
        --skip-k1) skip_k1=1 ;;
        --k3-build) build_k3=1 ;;
        --out)
            (($# >= 2)) || die "--out needs a file"
            out="$2"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
    shift
done
for n in "${K1_BUDGET_S}" "${K4_MIN}" "${K5_MIN_PERCENT}"; do
    [[ "${n}" =~ ^[0-9]+$ ]] || die "K1_BUDGET_S, K4_MIN and K5_MIN_PERCENT must be integers"
done

load_start="$(cut -d' ' -f1-3 /proc/loadavg)"
k1=""
if ((!skip_k1 || build_k3)); then
    if [[ -n "${DX_KPIS_DIR:-}" ]]; then
        work="$(realpath -m "${DX_KPIS_DIR}")"
        [[ "${work}/" != "${ROOT}/"* ]] || die "DX_KPIS_DIR ${work} is inside the repository; the K1 project must live outside it"
        mkdir -p "${work}"
    else
        work="$(mktemp -d)"
    fi
    if ((keep)); then
        echo "dx-kpis: keeping ${work}" >&2
    else
        # shellcheck disable=SC2016 # expanded when the trap runs
        trap 'rm -rf "${work:?}/svc" "${work:?}/target" "${work:?}/build.log" "${work:?}/svc.log" "${work:?}/k3" "${work:?}/k3-target" "${work:?}/k3.log"; [[ -n "${DX_KPIS_DIR:-}" ]] || rmdir "${work}" 2>/dev/null || true' EXIT
    fi
fi
if ((!skip_k1)); then
    echo "dx-kpis: building id-effect (untimed)" >&2
    (cd "${ROOT}" && cargo build --quiet -p id_effect_tool --bin id-effect) || fail "cargo build -p id_effect_tool"
    tool_target="${CARGO_TARGET_DIR:-target}"
    [[ "${tool_target}" == /* ]] || tool_target="${ROOT}/${tool_target}"
    bin="${tool_target}/debug/id-effect"
    [[ -x "${bin}" ]] || fail "no id-effect binary at ${bin}"
    k1="$(k1_measure "${work}" "${bin}")"
fi

[[ -f "${K3_PATCH}" ]] || fail "K3: no ${K3_PATCH}"
k3_applies "${K3_PATCH}" || fail "K3: ${K3_PATCH} does not apply to HEAD"
k3="$(k3_count "${K3_PATCH}")"
k3b=""
if ((build_k3)); then
    k3b="$(k3_build "${work}" "${K3_PATCH}")"
fi

k4="$(k4_count "${ROOT}/docs/diagnostics.md" "${ROOT}/crates/id_effect/tests/ui/gallery")" ||
fail "K4: docs/diagnostics.md and the gallery disagree"
included="$(mktemp)"
k5_included "${ROOT}/examples/book-snippets/src" "${ROOT}/crates/id_effect/src" >"${included}"
k5="$(k5_count "${ROOT}/crates/id_effect/book/src" "${included}")"
rm -f "${included}"

rev="$(git -C "${ROOT}" rev-parse --short HEAD 2>/dev/null || echo unknown)"
if [[ -n "$(git -C "${ROOT}" status --porcelain -- crates examples docs/diagnostics.md 2>/dev/null)" ]]; then
    rev+="+dirty"
fi
result="$(emit_json "${k1}" "${k4}" "${k5}" "$(machine_json "${load_start}")" "$(rustc -V)" "${rev}" "${k3}" "${k3b}")"
if [[ -n "${out}" || -n "${k1}" ]]; then
    out="${out:-${ROOT}/docs/benchmarks/dx-kpis-${RELEASE}.json}"
    printf '%s\n' "${result}" >"${out}"
    # The repository formats JSON with biome (treefmt.toml); keep the committed file stable.
    if command -v biome >/dev/null; then
        (cd "${ROOT}" && biome format --write "$(realpath "${out}")") >/dev/null 2>&1 || echo "dx-kpis: biome could not format ${out}" >&2
    fi
    echo "dx-kpis: wrote ${out}" >&2
fi

read -r fixed rows without <<<"${k4}"
read -r compiled fences _ _ <<<"${k5}"
k4_ok=$((fixed >= K4_MIN))
k5_ok="$(percent_ok "${compiled}" "${fences}" "${K5_MIN_PERCENT}")"
k1_ok=1
if [[ -n "${k1}" ]]; then
    read -r total _ build boot <<<"${k1}"
    k1_ok="$(at_most "${total}" "${K1_BUDGET_S}")"
fi
if ((json)); then
    printf '%s\n' "${result}"
else
    if [[ -n "${k1}" ]]; then
        echo "dx-kpis: K1 ${total}s (budget ${K1_BUDGET_S}s; build ${build}s, boot to /health ${boot}s) $(ok_word "${k1_ok}")"
    else
        echo "dx-kpis: K1 skipped"
    fi
    read -r k3_lines k3_model k3_migration k3_routes k3_policy k3_test k3_other k3_files <<<"${k3}"
    k3_detail="model ${k3_model}, migration ${k3_migration}, routes ${k3_routes}, policy ${k3_policy}, test ${k3_test}"
    ((k3_other == 0)) || k3_detail+=", other ${k3_other}"
    k3_detail+="; ${k3_files} files; job excluded: no jobs battery before 0.10"
    if [[ -n "${k3b}" ]]; then
        read -r k3_build_s _ k3_outcome <<<"${k3b}"
        k3_detail+="; patched Ledger built in ${k3_build_s}s, invoice tests ${k3_outcome}"
    fi
    echo "dx-kpis: K3 ${k3_lines} added non-blank lines for the invoice resource in Ledger (${k3_detail}) measured"
    echo "dx-kpis: K4 ${fixed}/${rows} gallery errors state the fix (min ${K4_MIN}; without: ${without}) $(ok_word "${k4_ok}")"
    echo "dx-kpis: K5 ${compiled}/${fences} rust fences compiled = $(percent "${compiled}" "${fences}")% (min ${K5_MIN_PERCENT}%) $(ok_word "${k5_ok}")"
fi
((k1_ok && k4_ok && k5_ok)) || fail "a KPI misses its target"
