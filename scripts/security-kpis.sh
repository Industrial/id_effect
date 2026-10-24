#!/usr/bin/env bash
# Compute the security KPIs of a release (0.11.0.md, 0.12.0.md and 0.13.0.md, security_kpis and
# the exit gate "Security KPIs") and record them in docs/security/kpis-<major>.<minor>.json.
#
# Release targets (configure_release; another release is misuse until it has a case there):
#   0.11  fuzz targets >= 8, compile-fail cases >= 15.
#   0.12  fuzz targets >= 13 with >= 5 not in docs/security/kpis-0.11.json; compile-fail cases
#         >= 0.11's count + 2, r1_rpc_procedure_without_access_decision and
#         r10_zero_rtt_on_mutation among them; the Security table of
#         .maestro/specs/releases/0.12.0.md (security_table) and the T12 bench gates of
#         docs/benchmarks/v0.12.json (t12_benches).
#   0.13  fuzz targets >= 22 with >= 5 not in docs/security/kpis-0.12.json and no crash input left
#         by a local run (fuzz_local_crashes); compile-fail cases >= 0.12's count + 2,
#         r1_admin_site_without_guard and r1_mcp_endpoint_without_access_decision among them; the
#         Security table of .maestro/specs/releases/0.13.0.md (security_table, which runs
#         tests/security_compile_fail/tests/surface_013.rs with the compile-fail suite); the
#         secure-stack and unary RPC deltas from docs/benchmarks/v0.13.json (secure_stack,
#         unary_rpc).
#
# Local KPIs (computed from the checkout; a miss is a FAIL):
#   open_highs           findings of docs/security/findings.json with severity high and status
#                        open (target 0). The register itself must be complete: every id of the
#                        2026-10-18 review (H1-H8, M1-M20, L1-L6, S1-S7) exactly once, a known
#                        status, every closed finding with a regression test or a gate, every
#                        accepted one with a reason (`note`), every named test found in the code
#                        (a word in crates/ tests/ scripts/ lints/ examples/ .github/ fuzz/, or a
#                        compile-fail case file of that name), every gate path present, every
#                        commit known to git.
#   adr_0013_enforcement the ten rules of docs/adrs/0013-secure-by-default.md ("The ten rules"):
#                        each row's "Enforcement point" cell names at least one backticked
#                        point and every one resolves: a path (`scripts/check-dangerous.sh`), a
#                        module (`id_effect::observability::posture` -> crates/id_effect/src/
#                        observability/posture.rs), a test family (`m4_*` -> a `fn m4_...`), or a
#                        test, lint or compile-fail case name (as for open_highs). Existence only:
#                        the tests themselves run in CI (moon run :ci, tests/security_compile_fail).
#   fuzz_targets         fuzz/fuzz_targets/*.rs that fuzz/Cargo.toml declares as a [[bin]] (so
#                        `cargo fuzz list`, which .clusterfuzzlite/build.sh builds, lists them),
#                        with .github/workflows/cflite_pr.yml and cflite_batch.yml present
#                        (target >= 8; 0.12: >= 13, and >= 5 of them `new`, not among the
#                        `targets` of the previous release's kpis JSON; 0.13: >= 22, >= 5 new).
#   fuzz_local_crashes   (0.13) the crash, leak, oom and timeout inputs libFuzzer wrote under
#                        fuzz/artifacts/<target>/ in this checkout (`cargo fuzz run`; the
#                        directory is git-ignored, so only local runs leave them; target 0). No
#                        fuzz/artifacts: "not measured". The CI side is fuzz_open_crashes.
#   compile_fail_cases   tests/security_compile_fail/ui/*.rs with a .stderr next to them
#                        (target >= 15; 0.12 and 0.13: >= the previous release's value + 2, and
#                        every `required` case there).
#   security_table       (0.12, 0.13) the "## Security" table of the release spec: every backticked
#                        name of its "Enforcement point" column exists, as a compile-fail case, an
#                        opt-out (dangerous_* / insecure_*, such as dangerous_any_origin: a word
#                        in the code; check_dangerous checks it records itself), or a `fn <name>`
#                        test. A dangerous_* / insecure_* name is a test, not an opt-out, when its
#                        `fn` carries a test attribute (`#[test]`, `#[tokio::test]`, ...) in the
#                        three lines above it (0.13: dangerous_tenant_from_header_is_recorded).
#                        A name with a fuzz/fuzz_targets/<name>.rs is a fuzz target and must be
#                        a [[bin]] of fuzz/Cargo.toml (0.13: fuzz `sql_page_cursor`; fuzz_targets
#                        counts them and ClusterFuzzLite runs them). Then the tests run green:
#                        `cargo nextest run --workspace --all-features` on exactly those tests
#                        (each must be listed by `cargo nextest list` under that filter, so a
#                        name compiled out by a feature FAILs), and `cargo nextest run -p
#                        security_compile_fail` for the cases (without --all-features:
#                        r8_plaintext_tcp_without_feature needs id_effect_node's
#                        insecure-plaintext-tcp off). SECURITY_KPIS_RUN_TESTS=0
#                        or --no-run-tests checks existence only and reports the run "not
#                        measured".
#   t12_benches          (0.12) the T12 gates recorded in docs/benchmarks/v0.12.json
#                        (`gates_0_12`, `soak`): unary delta <= 3000 ns, 50-call batch over 50
#                        sequential <= 0.20, HTTP/3 p99 over HTTP/2 p99 under loss <= 1.00, the
#                        10-minute soak of 10,000 streams with RSS end/minute 5 <= 1.05 and every
#                        heartbeat delivered in order (0 missing, 0 out of order, 0 streams short
#                        or ended early). The mark is minute 5 (0.12 F50/F60): the example's own
#                        mark when it is at 300 s or later, else the first sample at 300 s or
#                        later. The growth since minute 1 is reported, not gated
#                        (`rss_growth_minute_1`).
#   secure_stack         gate/axum_request_secure_stack - gate/axum_request_bare (median, ns) from
#                        the bench export SECURITY_KPIS_BENCH (default .tmp/bench-head.json, written
#                        by scripts/bench-run.sh + scripts/bench-export.sh; 0.12 without that file:
#                        docs/benchmarks/v0.12.json, the T12 run; 0.13: docs/benchmarks/v0.13.json
#                        unless SECURITY_KPIS_BENCH is set); target <= 5000 ns, the
#                        owner decision of 2026-10-19 (b66751f0; was ratio <= 1.15). The ratio is
#                        recorded for information. No export, or one without both ids: "not
#                        measured" (reported, not a FAIL). `stale` is true when crates/,
#                        Cargo.toml or Cargo.lock changed between the benched commit and HEAD
#                        (reported, not a FAIL).
#   unary_rpc            (0.13) rpc/unary_json - rpc/axum_json_handler (median, ns) from the same
#                        bench export as secure_stack; target <= 3000 ns (0.12 D5, the `delta`
#                        gate of docs/benchmarks/gates.toml). Missing as for secure_stack.
#   check_dangerous      scripts/check-dangerous.sh passes (ADR 0013 R10).
#   check_workflows      scripts/check-workflows.sh passes (S1, S2).
#   sha_pinned_actions   `uses:` of .github/workflows/*.yml that are not local (./): the share
#                        pinned by a 40-character commit SHA (or a docker @sha256 digest). The
#                        slsa-github-generator reusable workflow is the documented tag-pinned
#                        exception (scripts/check-workflows.sh); any other unpinned one FAILs.
#   registry_token       s3_no_registry_token_in_workflows: no CARGO_REGISTRY_TOKEN under .github.
#
# Online and owner KPIs (GitHub and crates.io, read-only; they exist only after the release is
# pushed and published). Not reachable or not there yet: "pending" with the reason (reported, not
# a FAIL); a measured value below its target is a FAIL:
#   adr_0013_status      the ADR's Status (owner acceptance, D22): Accepted = pass, otherwise
#                        pending. Read from the checkout, but an owner step, so it never FAILs.
#   openssf_scorecard    GET https://api.securityscorecards.dev/projects/github.com/<repo>, the
#                        result scorecard.yml publishes from main (target >= 7.5).
#   best_practices       the badge level of the bestpractices.dev project (target passing); the
#                        project id from SECURITY_KPIS_BESTPRACTICES_ID or a
#                        bestpractices.dev/projects/<n> link in README.md or docs/security/openssf.md.
#   fuzz_open_crashes    the latest cflite_batch.yml run on main (`gh run list`): success = 0 open
#                        crashes.
#   slsa                 Build L3 (slsa-github-generator in publish.yml); every crate of
#                        `scripts/validate-crate-packaging.sh --print-order` published at VERSION
#                        on crates.io, and the publish.yml run of tag v<VERSION> with its "Verify
#                        published crates against the provenance" job green (it runs slsa-verifier
#                        on every crate downloaded from crates.io) and the release carrying
#                        id_effect-<VERSION>.intoto.jsonl. Verified = published when it is green.
#
# Usage: bash scripts/security-kpis.sh [--out FILE] [--offline] [--no-run-tests] [VERSION]
#        bash scripts/security-kpis.sh --self-test
#   VERSION     the release, X.Y.Z (default: the workspace version in Cargo.toml)
#   --out FILE  where to write the JSON (default docs/security/kpis-<X.Y>.json)
#   --offline   do not query GitHub or crates.io: every online KPI is pending
#               (also SECURITY_KPIS_OFFLINE=1)
#   --no-run-tests  security_table checks that its tests exist but does not run them
#               (also SECURITY_KPIS_RUN_TESTS=0); the run is "not measured"
#   --self-test runs the kpi_* and s3_* cases below against fixtures in a temporary directory
#               (offline, no network).
# Environment: SECURITY_KPIS_BENCH (bench export), SECURITY_KPIS_REPO (default
# Industrial/id_effect), SECURITY_KPIS_BESTPRACTICES_ID.
# Output: one `security-kpis: <kpi> <summary> (target ...) pass|FAIL|pending|not measured` line
# per KPI, then the JSON file (formatted with biome when it is on PATH). Exit status: 0 when no
# KPI FAILed (pending and not measured do not fail), 1 on a FAIL, 2 on misuse.
# Requires: bash >= 4.4, jq, git, awk, grep, find; curl and gh for the online KPIs; cargo with
# cargo-nextest for the security_table run (cargo's environment: CARGO_TARGET_DIR and the rest).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
REPO_SLUG="${SECURITY_KPIS_REPO:-Industrial/id_effect}"
OFFLINE="${SECURITY_KPIS_OFFLINE:-0}"
UA="id_effect security-kpis (https://github.com/Industrial/id_effect)"
DELTA_LIMIT_NS=5000
FUZZ_MIN=8
COMPILE_FAIL_MIN=15
# Set per release by configure_release (the 0.11 values above are the defaults).
FUZZ_NEW_MIN=0
BASELINE=""
REQUIRED_CASES=()
SECURITY_SPEC=""
T12_BENCH=""
# 0.13: the release's own bench record (default export), the unary RPC delta and the local
# fuzz artifacts.
RELEASE_BENCH=""
UNARY_KPI=0
LOCAL_FUZZ_KPI=0
RUN_TABLE_TESTS="${SECURITY_KPIS_RUN_TESTS:-1}"
UNARY_LIMIT_NS=3000
BATCH_LIMIT=0.20
H3_LIMIT=1.00
SOAK_LIMIT=1.05
SCORECARD_MIN=7.5
SECURE_ID="gate/axum_request_secure_stack"
BARE_ID="gate/axum_request_bare"
UNARY_ID="rpc/unary_json"
HANDLER_ID="rpc/axum_json_handler"
VERIFY_JOB="Verify published crates against the provenance"
CODE_DIRS=(crates tests scripts lints examples .github fuzz)
EXPECTED_IDS=(H{1..8} M{1..20} L{1..6} S{1..7})
# A backtick, spelled without one so formatters do not read a command substitution.
BT="$(printf '\140')"

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed -E 's/^# ?//'
}

die() {
    echo "security-kpis: $*" >&2
    exit 2
}

is_git() {
    [[ -e "$1/.git" ]]
}

# The CODE_DIRS that exist under ROOT, one per line.
code_dirs() {
    local d
    for d in "${CODE_DIRS[@]}"; do
        [[ ! -e "$1/${d}" ]] || echo "${d}"
    done
}

# Succeeds when WORD is a word in the code under ROOT, or the name of a .rs file there (a
# compile-fail case). Tracked and untracked (not ignored) files.
code_has_word() {
    local root="$1" word="$2" dirs=()
    mapfile -t dirs < <(code_dirs "${root}")
    ((${#dirs[@]})) || return 1
    if is_git "${root}"; then
        git -C "${root}" grep -qw --untracked -e "${word}" -- "${dirs[@]}" && return 0
        git -C "${root}" ls-files -co --exclude-standard -- "${dirs[@]}" | grep -q "/${word}\.rs$"
    else
        (cd "${root}" && grep -rqw --exclude-dir=target -e "${word}" -- "${dirs[@]}") && return 0
        (cd "${root}" && find "${dirs[@]}" -name target -prune -o -name "${word}.rs" -print | grep -q .)
    fi
}

# Succeeds when the code under ROOT defines a function whose name starts with PREFIX.
code_has_fn_prefix() {
    local root="$1" prefix="$2" dirs=()
    mapfile -t dirs < <(code_dirs "${root}")
    ((${#dirs[@]})) || return 1
    if is_git "${root}"; then
        git -C "${root}" grep -qE --untracked -e "fn ${prefix}[A-Za-z0-9_]*" -- "${dirs[@]}"
    else
        (cd "${root}" && grep -rqE --exclude-dir=target -e "fn ${prefix}[A-Za-z0-9_]*" -- "${dirs[@]}")
    fi
}

# Succeeds when an ADR enforcement point TOKEN exists under ROOT (rules in the header).
resolve_token() {
    local root="$1" token="$2" crate rest
    if [[ "${token}" == */* ]]; then
        [[ -e "${root}/${token}" ]]
        return
    fi
    if [[ "${token}" == *::* ]]; then
        crate="${token%%::*}"
        rest="${token#*::}"
        rest="${rest//:://}"
        [[ -f "${root}/crates/${crate}/src/${rest}.rs" || -f "${root}/crates/${crate}/src/${rest}/mod.rs" ]]
        return
    fi
    if [[ "${token}" =~ ^[A-Za-z0-9_]+_\*$ ]]; then
        code_has_fn_prefix "${root}" "${token%\*}"
        return
    fi
    [[ "${token}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || return 1
    code_has_word "${root}" "${token}"
}

# The workspace version of ROOT/Cargo.toml ([workspace.package]).
workspace_version() {
    awk '/^\[workspace\.package\]/ { s = 1; next }
        /^\[/ { s = 0 }
        s && /^version *=/ { gsub(/[" ]/, ""); sub(/^version=/, ""); print; exit }' "$1/Cargo.toml" 2>/dev/null || true
}

# The targets of release X.Y (header, "Release targets") for the checkout ROOT.
configure_release() {
    local root="$1" release="$2" base
    case "${release}" in
        0.11) ;;
        0.12)
            FUZZ_MIN=13
            FUZZ_NEW_MIN=5
            BASELINE="${root}/docs/security/kpis-0.11.json"
            base="$(jq -er '.kpis.compile_fail_cases.value' "${BASELINE}" 2>/dev/null)" ||
            die "no compile_fail_cases.value in ${BASELINE#"${root}"/} (the 0.12 baseline)"
            COMPILE_FAIL_MIN=$((base + 2))
            REQUIRED_CASES=(r1_rpc_procedure_without_access_decision r10_zero_rtt_on_mutation)
            SECURITY_SPEC="${root}/.maestro/specs/releases/0.12.0.md"
            T12_BENCH="${root}/docs/benchmarks/v0.12.json"
            ;;
        0.13)
            FUZZ_MIN=22
            FUZZ_NEW_MIN=5
            BASELINE="${root}/docs/security/kpis-0.12.json"
            base="$(jq -er '.kpis.compile_fail_cases.value' "${BASELINE}" 2>/dev/null)" ||
            die "no compile_fail_cases.value in ${BASELINE#"${root}"/} (the 0.13 baseline)"
            COMPILE_FAIL_MIN=$((base + 2))
            REQUIRED_CASES=(r1_admin_site_without_guard r1_mcp_endpoint_without_access_decision)
            SECURITY_SPEC="${root}/.maestro/specs/releases/0.13.0.md"
            RELEASE_BENCH="${root}/docs/benchmarks/v0.13.json"
            UNARY_KPI=1
            LOCAL_FUZZ_KPI=1
            ;;
        *) die "no KPI targets for release ${release} (add a case to configure_release)" ;;
    esac
}

# --- local KPIs: each prints one JSON object with at least `status`, `target` and `summary` --

# The findings register FILE: open highs and the register's problems.
kpi_open_highs() {
    local root="$1" file="$2" id severity status kind value seen=" " n=0 open_highs=0
    local closed=0 accepted=0 open=0 problems=()
    if ! jq -e '.findings | type == "array"' "${file}" >/dev/null 2>&1; then
        jq -n --arg f "${file#"${root}"/}" '{value: null, target: 0, status: "fail",
            summary: "no findings array in \($f)", problems: ["no findings array in \($f)"]}'
        return
    fi
    while IFS=$'\t' read -r id severity status; do
        n=$((n + 1))
        [[ "${seen}" != *" ${id} "* ]] || problems+=("${id}: listed twice")
        seen+="${id} "
        case "${status}" in
            closed) closed=$((closed + 1)) ;;
            accepted) accepted=$((accepted + 1)) ;;
            open) open=$((open + 1)) ;;
            *) problems+=("${id}: unknown status '${status}'") ;;
        esac
        [[ "${severity}" != high || "${status}" != open ]] || open_highs=$((open_highs + 1))
    done < <(jq -r '.findings[] | [.id, .severity, .status] | @tsv' "${file}")
    for id in "${EXPECTED_IDS[@]}"; do
        [[ "${seen}" == *" ${id} "* ]] || problems+=("${id}: missing from the register")
    done
    local checks
    read -r -d '' checks <<'JQ' || true
        .findings[] | . as $f
        | (((.tests // [])[] | [$f.id, "test", .]),
           ((.gates // [])[] | [$f.id, "gate", .]),
           ((.commits // [])[] | [$f.id, "commit", .]),
           (if .status == "closed" and (((.tests // []) + (.gates // [])) | length) == 0
            then [$f.id, "untested", "-"] else empty end),
           (if .status == "accepted" and ((.note // "") == "")
            then [$f.id, "unreasoned", "-"] else empty end))
        | @tsv
JQ
    while IFS=$'\t' read -r id kind value; do
        case "${kind}" in
            test) code_has_word "${root}" "${value}" || problems+=("${id}: regression test ${value} not found in the code") ;;
            gate) [[ -e "${root}/${value}" ]] || problems+=("${id}: gate ${value} does not exist") ;;
            commit)
                if is_git "${root}" && ! git -C "${root}" cat-file -e "${value}^{commit}" 2>/dev/null; then
                    problems+=("${id}: commit ${value} unknown to git")
                fi
                ;;
            untested) problems+=("${id}: closed without a regression test or gate") ;;
            unreasoned) problems+=("${id}: accepted without a reason (note)") ;;
        esac
    done < <(jq -r "${checks}" "${file}")
    jq -n --argjson v "${open_highs}" --argjson n "${n}" --argjson closed "${closed}" \
    --argjson accepted "${accepted}" --argjson open "${open}" --arg f "${file#"${root}"/}" '
        $ARGS.positional as $p
        | {value: $v, target: 0,
           status: (if $v == 0 and ($p | length) == 0 then "pass" else "fail" end),
           register: $f, findings: $n, closed: $closed, accepted: $accepted, open: $open,
           problems: $p,
           summary: "\($v) open highs; \($n) findings: \($closed) closed, \($accepted) accepted, \($open) open; \($p | length) register problems"}' \
        --args "${problems[@]}"
}

# The ten rules of ADR file ADR: every rule names an enforcement point that exists.
kpi_adr_enforcement() {
    local root="$1" adr="$2" row rule cell token ok n=0 enforced=0 i problems=() tokens=() rules=" "
    if [[ ! -f "${adr}" ]]; then
        jq -n --arg f "${adr#"${root}"/}" \
            '{value: 0, target: 10, status: "fail", summary: "\($f) missing", problems: ["\($f) missing"]}'
        return
    fi
    while IFS= read -r row; do
        rule="$(awk -F'|' '{ gsub(/[ \t]/, "", $2); print $2 }' <<<"${row}")"
        cell="$(awk -F'|' '{ print $5 }' <<<"${row}")"
        n=$((n + 1))
        rules+="${rule} "
        mapfile -t tokens < <(grep -o "${BT}[^${BT}]*${BT}" <<<"${cell}" | tr -d "${BT}" || true)
        if ((${#tokens[@]} == 0)); then
            problems+=("${rule}: no enforcement point")
            continue
        fi
        ok=1
        for token in "${tokens[@]}"; do
            if ! resolve_token "${root}" "${token}"; then
                problems+=("${rule}: enforcement point ${token} not found")
                ok=0
            fi
        done
        ((ok)) && enforced=$((enforced + 1))
    done < <(awk '/^### The ten rules/ { s = 1; next } s && /^##/ { exit } s && /^\| R[0-9]+ \|/' "${adr}")
    for i in {1..10}; do
        [[ "${rules}" == *" R${i} "* ]] || problems+=("R${i}: no row in the rules table")
    done
    jq -n --argjson v "${enforced}" --argjson n "${n}" --arg f "${adr#"${root}"/}" '
        $ARGS.positional as $p
        | {value: $v, target: 10, rules: $n, adr: $f,
           status: (if $v == 10 and $n == 10 and ($p | length) == 0 then "pass" else "fail" end),
           problems: $p,
           summary: "\($v)/\($n) rules with an enforcement point that exists"}' --args "${problems[@]}"
}

# The Status word of ADR file ADR (owner acceptance, D22).
kpi_adr_status() {
    local word
    word="$(awk '/^## Status/ { s = 1; next } s && NF { print $1; exit }' "$1" 2>/dev/null || true)"
    word="${word%%[^A-Za-z]*}"
    jq -n --arg w "${word}" '
        {value: $w, target: "Accepted",
         status: (if $w == "Accepted" then "pass" else "pending" end),
         reason: (if $w == "Accepted" then null else "owner acceptance (0.11.0 D22)" end),
         summary: "ADR 0013 status \(if $w == "" then "unknown" else $w end)"}'
}

# The fuzz targets of ROOT; with BASELINE (a kpis JSON of the previous release), the ones not in
# its fuzz_targets.targets are `new` and at least FUZZ_NEW_MIN of them are needed.
kpi_fuzz_targets() {
    local root="$1" baseline="${2:-}" f name listed=() unlisted=() problems=() base="[]"
    if [[ -n "${baseline}" ]]; then
        base="$(jq -ce '.kpis.fuzz_targets.targets | arrays' "${baseline}" 2>/dev/null)" || {
            base="[]"
            problems+=("no fuzz_targets.targets in the baseline ${baseline#"${root}"/}")
        }
    fi
    shopt -s nullglob
    for f in "${root}"/fuzz/fuzz_targets/*.rs; do
        name="$(basename "${f}" .rs)"
        if grep -qF "path = \"fuzz_targets/${name}.rs\"" "${root}/fuzz/Cargo.toml" 2>/dev/null; then
            listed+=("${name}")
        else
            unlisted+=("${name}")
        fi
    done
    shopt -u nullglob
    for f in cflite_pr.yml cflite_batch.yml; do
        [[ -f "${root}/.github/workflows/${f}" ]] || problems+=(".github/workflows/${f} missing")
    done
    grep -q 'cargo fuzz list' "${root}/.clusterfuzzlite/build.sh" 2>/dev/null ||
    problems+=(".clusterfuzzlite/build.sh does not build every target (cargo fuzz list)")
    ((${#unlisted[@]} == 0)) || problems+=("not a [[bin]] of fuzz/Cargo.toml: ${unlisted[*]}")
    jq -n --argjson min "${FUZZ_MIN}" --argjson new_min "${FUZZ_NEW_MIN}" --arg listed "${listed[*]}" \
    --argjson base "${base}" --arg baseline "${baseline#"${root}"/}" '
        $ARGS.positional as $p
        | ($listed | split(" ") | map(select(. != ""))) as $t
        | ($t - $base) as $new
        | {value: ($t | length),
           target: (">= \($min)" + (if $new_min > 0 then " (>= \($new_min) new)" else "" end)),
           targets: $t, problems: $p}
        + (if $baseline == "" then {} else {baseline: $baseline, new: $new, new_count: ($new | length), new_min: $new_min} end)
        + {status: (if ($t | length) >= $min and ($new | length) >= $new_min and ($p | length) == 0 then "pass" else "fail" end),
           summary: ("\($t | length) cargo-fuzz targets under ClusterFuzzLite"
             + (if $baseline == "" then "" else ", \($new | length) new: \($new | join(" "))" end))}' --args "${problems[@]}"
}

# The crash inputs local fuzz runs left under ROOT/fuzz/artifacts (rules in the header).
kpi_fuzz_local_crashes() {
    local root="$1" dir="$1/fuzz/artifacts" files=() targets=()
    if [[ ! -d "${dir}" ]]; then
        jq -n '{value: null, target: "0", status: "not measured",
            reason: "no fuzz/artifacts: no local cargo fuzz run in this checkout",
            summary: "local fuzz crashes not measured"}'
        return
    fi
    mapfile -t targets < <(find "${dir}" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort)
    mapfile -t files < <(find "${dir}" -type f \( -name 'crash-*' -o -name 'leak-*' -o -name 'oom-*' \
        -o -name 'timeout-*' \) -printf '%P\n' | sort)
    jq -n --arg targets "${targets[*]}" '
        ($targets | split(" ") | map(select(. != ""))) as $t
        | $ARGS.positional as $f
        | {value: ($f | length), target: "0", targets_run: $t, inputs: $f,
           status: (if ($f | length) == 0 then "pass" else "fail" end),
           summary: "\($f | length) crash inputs under fuzz/artifacts (local runs of \($t | length) targets)"}' \
        --args "${files[@]}"
}

kpi_compile_fail_cases() {
    local root="$1" f n=0 problems=()
    shopt -s nullglob
    for f in "${root}"/tests/security_compile_fail/ui/*.rs; do
        if [[ -f "${f%.rs}.stderr" ]]; then
            n=$((n + 1))
        else
            problems+=("$(basename "${f}") has no .stderr")
        fi
    done
    shopt -u nullglob
    for f in "${REQUIRED_CASES[@]}"; do
        [[ -f "${root}/tests/security_compile_fail/ui/${f}.rs" ]] || problems+=("required case ${f} missing")
    done
    jq -n --argjson v "${n}" --argjson min "${COMPILE_FAIL_MIN}" --arg required "${REQUIRED_CASES[*]}" '
        $ARGS.positional as $p
        | ($required | split(" ") | map(select(. != ""))) as $r
        | {value: $v, target: (">= \($min)" + (if ($r | length) > 0 then ", including \($r | join(" "))" else "" end)),
           problems: $p}
        + (if ($r | length) > 0 then {required: $r} else {} end)
        + {status: (if $v >= $min and ($p | length) == 0 then "pass" else "fail" end),
           summary: "\($v) compile-fail cases in tests/security_compile_fail/ui"}' --args "${problems[@]}"
}

# --- security_table (0.12) ------------------------------------------------------------------

# Prints `<threat>\t<name>` for every backticked name in the "Enforcement point" column (the
# fourth) of the table under "## Security" of the release spec SPEC.
security_table_names() {
    awk -F'|' '/^## Security/ { s = 1; next } s && /^## / { exit }
        s && /^\| / && $2 !~ /^ *Threat *$/ { print $2 "\t" $5 }' "$1" |
    while IFS=$'\t' read -r threat cell; do
        threat="$(sed -E 's/^ +| +$//g' <<<"${threat}")"
        grep -o "${BT}[^${BT}]*${BT}" <<<"${cell}" | tr -d "${BT}" |
        grep -E '^[A-Za-z_][A-Za-z0-9_]*$' | awk -v t="${threat}" '{ print t "\t" $0 }' || true
    done
}

# Succeeds when the code under ROOT defines `fn NAME`.
code_has_fn() {
    local root="$1" name="$2" dirs=()
    mapfile -t dirs < <(code_dirs "${root}")
    ((${#dirs[@]})) || return 1
    if is_git "${root}"; then
        git -C "${root}" grep -qE --untracked -e "fn ${name}\b" -- "${dirs[@]}"
    else
        (cd "${root}" && grep -rqE --exclude-dir=target -e "fn ${name}\b" -- "${dirs[@]}")
    fi
}

# Succeeds when the code under ROOT defines `fn NAME` with a test attribute (`#[test]`,
# `#[tokio::test]`, `#[tokio::test(...)]`, ...) in the three lines above it.
code_has_test_fn() {
    local root="$1" name="$2" dirs=() hits
    mapfile -t dirs < <(code_dirs "${root}")
    ((${#dirs[@]})) || return 1
    if is_git "${root}"; then
        hits="$(git -C "${root}" grep -h --untracked -B3 -E -e "fn ${name}\b" -- "${dirs[@]}" || true)"
    else
        hits="$(cd "${root}" && grep -rh --exclude-dir=target -B3 -E -e "fn ${name}\b" -- "${dirs[@]}" || true)"
    fi
    grep -qE '^[[:space:]]*#\[([A-Za-z_][A-Za-z0-9_]*::)*test(\]|\()' <<<"${hits}"
}

# The nextest filterset of exactly the tests NAME.. (a test's full name is NAME or ends in ::NAME).
tests_filter() {
    local IFS='|'
    printf 'test(/(^|::)(%s)$/)' "$*"
}

# The cargo calls of the run, one function each so the self-test can stand in for them. Every
# one runs in ROOT and writes its output to LOG.
# table_list ROOT LOG FILTER: prints the names of the tests FILTER selects (--all-features).
table_list() {
    (cd "$1" && cargo nextest list --workspace --all-features -E "$3" --message-format json 2>"$2") |
    jq -r '."rust-suites"[] | .testcases | to_entries[]
        | select(.value."filter-match".status == "matches") | .key'
}

# table_run ROOT LOG FILTER: runs the tests FILTER selects (--all-features).
table_run() {
    (cd "$1" && cargo nextest run --workspace --all-features --no-fail-fast -E "$3") >"$2" 2>&1
}

# cases_run ROOT LOG: runs the compile-fail suite (its own features; see the header).
cases_run() {
    (cd "$1" && cargo nextest run -p security_compile_fail --no-fail-fast) >"$2" 2>&1
}

# The Security table of SPEC: every name exists; then (RUN_TABLE_TESTS=1) its tests and the
# compile-fail suite run green. Logs go to TMP.
kpi_security_table() {
    local root="$1" spec="$2" tmp="$3" threat name rows=0 problems=() tests=() cases=() points=() fuzz=()
    local listed=() failed=() run="not run" ran=0 t found
    if [[ ! -f "${spec}" ]]; then
        jq -n --arg f "${spec#"${root}"/}" '{value: null, target: "every Security-table test green",
            status: "fail", summary: "\($f) missing", problems: ["\($f) missing"]}'
        return
    fi
    while IFS=$'\t' read -r threat name; do
        rows=$((rows + 1))
        if [[ -f "${root}/tests/security_compile_fail/ui/${name}.rs" ]]; then
            cases+=("${name}")
        elif [[ -f "${root}/fuzz/fuzz_targets/${name}.rs" ]]; then
            if grep -qF "path = \"fuzz_targets/${name}.rs\"" "${root}/fuzz/Cargo.toml" 2>/dev/null; then
                fuzz+=("${name}")
            else
                problems+=("${threat}: fuzz target ${name} is not a [[bin]] of fuzz/Cargo.toml")
            fi
        elif [[ "${name}" =~ ^(dangerous|insecure)_ ]] && code_has_test_fn "${root}" "${name}"; then
            tests+=("${name}")
        elif [[ "${name}" =~ ^(dangerous|insecure)_ ]] && code_has_word "${root}" "${name}"; then
            points+=("${name}")
        elif code_has_fn "${root}" "${name}"; then
            tests+=("${name}")
        else
            problems+=("${threat}: ${name} not found in the code")
        fi
    done < <(security_table_names "${spec}")
    ((rows > 0)) || problems+=("no enforcement point names in the Security table of ${spec#"${root}"/}")
    if ((RUN_TABLE_TESTS)) && ((${#problems[@]} == 0)); then
        run="green"
        ran=1
        if ((${#tests[@]})); then
            mapfile -t listed < <(table_list "${root}" "${tmp}/table-list.log" "$(tests_filter "${tests[@]}")" || true)
            for t in "${tests[@]}"; do
                found=0
                for name in "${listed[@]}"; do
                    [[ "${name}" != "${t}" && "${name}" != *"::${t}" ]] || found=1
                done
                ((found)) || problems+=("${t}: not a test of the --all-features workspace build (cargo nextest list)")
            done
            if ! table_run "${root}" "${tmp}/table-run.log" "$(tests_filter "${tests[@]}")"; then
                run="failed"
                mapfile -t failed < <(grep -E '^ +(FAIL|TIMEOUT|SIGSEGV|SIGABRT) ' "${tmp}/table-run.log" | awk '{ print $NF }' | sort -u || true)
                problems+=("the Security-table tests failed: ${failed[*]:-see cargo nextest run}")
            fi
        fi
        if ((${#cases[@]})) && ! cases_run "${root}" "${tmp}/cases-run.log"; then
            run="failed"
            problems+=("the compile-fail suite (cargo nextest run -p security_compile_fail) failed")
        fi
    fi
    jq -n --argjson rows "${rows}" --arg tests "${tests[*]}" --arg cases "${cases[*]}" \
        --arg points "${points[*]}" --arg run "${run}" --argjson ran "${ran}" --argjson listed "${#listed[@]}" \
    --arg fuzz "${fuzz[*]}" --arg spec "${spec#"${root}"/}" '
        def words: split(" ") | map(select(. != ""));
        $ARGS.positional as $p
        | ($tests | words) as $t | ($cases | words) as $c | ($points | words) as $o | ($fuzz | words) as $f
        | {value: (if $ran == 1 then $run else null end), target: "every Security-table test green",
           spec: $spec, names: $rows, tests: $t, compile_fail_cases: $c, other_points: $o,
           listed: $listed, run: $run, problems: $p}
        + (if ($f | length) > 0 then {fuzz_targets: $f} else {} end)
        + {status: (if ($p | length) > 0 then "fail" elif $ran == 1 then "pass" else "not measured" end),
           summary: ("\($rows) names in the Security table: \($t | length) tests, \($c | length) compile-fail cases, "
             + (if ($f | length) > 0 then "\($f | length) fuzz targets, " else "" end)
             + "\($o | length) other points"
             + (if ($p | length) > 0 then "; \($p | length) problems"
                elif $ran == 1 then "; tests and compile-fail suite green"
                else "; all exist, run not measured (--no-run-tests)" end))}' --args "${problems[@]}"
}

# --- t12_benches (0.12) ---------------------------------------------------------------------

# The T12 gates of the bench record BENCH (docs/benchmarks/v0.12.json).
kpi_t12_benches() {
    local root="$1" bench="$2"
    if [[ ! -f "${bench}" ]]; then
        jq -n --arg b "${bench#"${root}"/}" '{value: null, target: "every T12 gate", status: "fail",
            summary: "\($b) missing", problems: ["\($b) missing"]}'
        return
    fi
    jq --arg src "${bench#"${root}"/}" --argjson unary "${UNARY_LIMIT_NS}" --argjson batch "${BATCH_LIMIT}" \
    --argjson h3 "${H3_LIMIT}" --argjson soak "${SOAK_LIMIT}" '
        def r(n): if . == null then null else (. * n | round) / n end;
        .gates_0_12 as $g | .soak as $s
        | ([$s.rss_kib.samples[]? | select(.[0] >= 300)] | first | .[1]) as $m5
        | ([$s.rss_kib.samples[]? | select(.[0] >= 60)] | first | .[1]) as $m1
        | (if ($s.rss_kib.mark_s // 0) >= 300 then $s.rss_kib.growth
           elif ($m5 // 0) > 0 and $s.rss_kib.end != null then $s.rss_kib.end / $m5
           else null end) as $gated
        | (if $s.rss_kib.mark_s == 60 then $s.rss_kib.growth
           elif ($m1 // 0) > 0 and $s.rss_kib.end != null then $s.rss_kib.end / $m1
           else null end) as $growth1
        | {unary_delta_ns: {value: ($g.unary_delta_ns | r(1)), limit: $unary,
                            pass: ($g.unary_delta_ns != null and $g.unary_delta_ns <= $unary),
                            ratio: ($g.unary_ratio | r(100)), before_ns: ($g.unary_before_ns | r(1))},
           batch_over_sequential: {value: ($g.batch_over_sequential | r(1000)), limit: $batch,
                                   pass: ($g.batch_over_sequential != null and $g.batch_over_sequential <= $batch),
                                   speedup: (if ($g.batch_over_sequential // 0) > 0 then (1 / $g.batch_over_sequential | r(10)) else null end)},
           h3_over_h2_p99: {value: ($g.h3_over_h2_p99 | r(1000)), limit: $h3,
                            pass: ($g.h3_over_h2_p99 != null and $g.h3_over_h2_p99 <= $h3), mode: $g.h3_mode},
           soak: {streams: $s.streams, duration_s: $s.duration_s, rss_growth: ($gated | r(1000)),
                  rss_growth_minute_1: ($growth1 | r(1000)),
                  limit: $soak, mark_s: $s.rss_kib.mark_s, rss_at_mark_kib: $s.rss_kib.at_mark,
                  rss_end_kib: $s.rss_kib.end, rss_peak_kib: $s.rss_kib.peak,
                  rss_at_300s_kib: $m5,
                  rss_growth_after_300s: (if ($m5 // 0) > 0 and $s.rss_kib.end != null then ($s.rss_kib.end / $m5 | r(1000)) else null end),
                  heartbeats_received: $s.heartbeats_received, heartbeats_missing: $s.heartbeats_missing,
                  heartbeats_out_of_order: $s.heartbeats_out_of_order, streams_short: $s.streams_short,
                  streams_ended_early: $s.streams_ended_early,
                  pass: ($s.streams >= 10000 and $s.duration_s >= 600 and $gated != null and $gated <= $soak
                         and $s.heartbeats_missing == 0 and $s.heartbeats_out_of_order == 0
                         and $s.streams_short == 0 and $s.streams_ended_early == 0)}} as $k
        | ([$k | to_entries[] | select(.value.pass != true) | .key]) as $failed
        | $k + {value: (4 - ($failed | length)), target: "4/4 T12 gates", source: $src, sha: .sha,
                created: .created, failed: $failed,
                status: (if ($failed | length) == 0 then "pass" else "fail" end),
                summary: "unary +\($k.unary_delta_ns.value) ns, batch \($k.batch_over_sequential.value) of sequential, h3/h2 p99 \($k.h3_over_h2_p99.value), soak \($k.soak.streams) streams RSS \($k.soak.rss_growth)x after minute 5 (\($k.soak.rss_growth_minute_1)x after minute 1, reported), \($k.soak.heartbeats_missing) heartbeats missing"}' "${bench}"
}

# A median delta of the bench export BENCH (a path; shown relative to ROOT): ID over BASE (bench
# ids) against LIMIT ns. NAME labels the summary, KEY and BASE_KEY name the two medians in the
# JSON, ONLY is the `bench-run.sh --only` that measures both, NOTE explains the ratio.
kpi_bench_delta() {
    local root="$1" bench="$2" id="$3" base="$4" limit="$5" name="$6" key="$7" base_key="$8" only="$9"
    local note="${10}" sha stale=false
    if [[ ! -f "${bench}" ]]; then
        jq -n --arg b "${bench#"${root}"/}" --argjson limit "${limit}" --arg name "${name}" --arg only "${only}" '
            {value: null, target: "<= \($limit) ns", status: "not measured", source: $b,
             reason: "no bench export (scripts/bench-run.sh --only \($only), then scripts/bench-export.sh head \($b))",
             summary: "\($name) not measured"}'
        return
    fi
    # Stale: the crates or the lock file changed between the benched commit and HEAD.
    sha="$(jq -r '.sha // empty' "${bench}" 2>/dev/null || true)"
    if is_git "${root}" && [[ -n "${sha}" ]] &&
    ! git -C "${root}" diff --quiet "${sha}" HEAD -- crates Cargo.toml Cargo.lock 2>/dev/null; then
        stale=true
    fi
    jq --arg s "${id}" --arg b "${base}" --argjson limit "${limit}" --arg name "${name}" \
        --arg key "${key}" --arg base_key "${base_key}" --arg note "${note}" \
    --arg src "${bench#"${root}"/}" --argjson stale "${stale}" '
        .benches[$s].median as $sv
        | .benches[$b].median as $bv
        | if ($sv == null or $bv == null) then
            {value: null, target: "<= \($limit) ns", status: "not measured", source: $src,
             reason: "the export has no \($s) or \($b)", summary: "\($name) not measured"}
          else
            (($sv - $bv) | round) as $d
            | (($sv / $bv * 100 | round) / 100) as $ratio
            | {value: $d, unit: "ns", target: "<= \($limit) ns",
               status: (if $d <= $limit then "pass" else "fail" end),
               ($key): (($sv * 10 | round) / 10), ($base_key): (($bv * 10 | round) / 10),
               ratio: $ratio, ratio_note: $note,
               metric: "median", bench: $s, over: $b, source: $src, sha: .sha, created: .created,
               machine: .machine, stale: $stale,
               summary: "+\($d) ns \($name) (\($bv | round) -> \($sv | round) ns, ratio \($ratio))"}
          end' "${bench}"
}

# The secure-stack overhead from the bench export BENCH.
kpi_secure_stack() {
    kpi_bench_delta "$1" "$2" "${SECURE_ID}" "${BARE_ID}" "${DELTA_LIMIT_NS}" "secure stack over bare" \
        secure_stack_ns bare_ns id_effect_axum:gate_request \
        "informational; the gate is the absolute budget (owner decision 2026-10-19, b66751f0; was <= 1.15x)"
}

# The unary RPC overhead over the raw JSON handler from the bench export BENCH (0.12 D5).
kpi_unary_rpc() {
    kpi_bench_delta "$1" "$2" "${UNARY_ID}" "${HANDLER_ID}" "${UNARY_LIMIT_NS}" \
        "unary RPC over the raw JSON handler" unary_json_ns axum_json_handler_ns id_effect_rpc:gate_rpc \
        "informational; the gate is the absolute budget (0.12 D5, gates.toml delta rpc/unary_json)"
}

# Runs ROOT/SCRIPT; pass when it exits 0.
kpi_script() {
    local root="$1" script="$2" out status=pass
    out="$(bash "${root}/${script}" 2>&1)" || status=fail
    jq -n --arg s "${script}" --arg st "${status}" --arg last "$(tail -n 1 <<<"${out}")" \
        '{value: $st, target: "pass", status: $st, script: $s, output: $last, summary: "\($s): \($last)"}'
}

kpi_sha_pinned_actions() {
    local root="$1" files=() counts total pinned exempt
    shopt -s nullglob
    files=("${root}"/.github/workflows/*.yml "${root}"/.github/workflows/*.yaml)
    shopt -u nullglob
    if ((${#files[@]} == 0)); then
        jq -n '{value: null, target: "100 %", status: "fail", summary: "no workflows"}'
        return
    fi
    counts="$(awk '
        /^[ \t]*#/ { next }
        /^[ \t]*(-[ \t]+)?uses[ \t]*:/ {
            v = $0
            sub(/^[ \t]*(-[ \t]+)?uses[ \t]*:[ \t]*/, "", v)
            sub(/[ \t]+#.*$/, "", v)
            gsub(/[\042\047]/, "", v)
            sub(/[ \t\r]+$/, "", v)
            if (v ~ /^\.\.?\//) next
            total++
            ref = v
            sub(/.*@/, "", ref)
            if (v ~ /^docker:\/\// && v ~ /@sha256:[0-9a-f]+$/) pinned++
            else if (v !~ /^docker:/ && length(ref) == 40 && ref ~ /^[0-9a-f]+$/) pinned++
            else if (v ~ /^slsa-framework\/slsa-github-generator\//) exempt++
        }
        END { printf "%d %d %d\n", total, pinned, exempt }' "${files[@]}")"
    read -r total pinned exempt <<<"${counts}"
    jq -n --argjson t "${total}" --argjson p "${pinned}" --argjson e "${exempt}" '
        (if $t == 0 then 100 else ($p * 1000 / $t | round) / 10 end) as $pct
        | {value: $pct, unit: "%", target: "100 % (the slsa-github-generator reusable workflow excepted)",
           uses: $t, sha_pinned: $p, tag_pinned_exception: $e,
           status: (if $t - $p - $e == 0 then "pass" else "fail" end),
           summary: "\($p)/\($t) uses SHA-pinned (\($pct) %), \($e) documented exception, \($t - $p - $e) unpinned"}'
}

# S3: no workflow reads a long-lived crates.io token.
s3_no_registry_token_in_workflows() {
    local root="$1" hits
    hits="$(grep -rln CARGO_REGISTRY_TOKEN "${root}/.github" 2>/dev/null || true)"
    jq -n --arg hits "${hits//"${root}"\//}" '
        ($hits | split("\n") | map(select(. != ""))) as $h
        | {value: ($h | length), target: 0, files: $h,
           status: (if ($h | length) == 0 then "pass" else "fail" end),
           summary: "\($h | length) files under .github mention CARGO_REGISTRY_TOKEN"}'
}

# --- online KPIs -----------------------------------------------------------------------------

# Prints a pending KPI: REASON TARGET NAME.
pending() {
    jq -n --arg r "$1" --arg t "$2" --arg s "$3" \
        '{value: null, target: $t, status: "pending", reason: $r, summary: "\($s) pending: \($r)"}'
}

# GET URL into FILE; prints the HTTP status (000 when unreachable).
http_get() {
    curl -sS -m 20 -A "${UA}" -o "$2" -w '%{http_code}' "$1" 2>/dev/null || echo 000
}

kpi_scorecard() {
    local tmp="$1" code
    if ((OFFLINE)); then
        pending "offline" ">= ${SCORECARD_MIN}" "OpenSSF Scorecard"
        return
    fi
    code="$(http_get "https://api.securityscorecards.dev/projects/github.com/${REPO_SLUG}" "${tmp}/scorecard.json")"
    case "${code}" in
        200)
            jq --argjson min "${SCORECARD_MIN}" '
                {value: .score, target: ">= \($min)", status: (if .score >= $min then "pass" else "fail" end),
                 date: .date, commit: .repo.commit,
                 checks: ([.checks[]? | {key: .name, value: .score}] | from_entries),
                 source: "api.securityscorecards.dev",
                 summary: "Scorecard \(.score) (\(.date))"}' "${tmp}/scorecard.json"
            ;;
        404)
            pending "no published Scorecard result yet (scorecard.yml publishes from main: needs the release push)" \
                ">= ${SCORECARD_MIN}" "OpenSSF Scorecard"
            ;;
        *) pending "api.securityscorecards.dev answered HTTP ${code}" ">= ${SCORECARD_MIN}" "OpenSSF Scorecard" ;;
    esac
}

kpi_best_practices() {
    local root="$1" tmp="$2" id="${SECURITY_KPIS_BESTPRACTICES_ID:-}" code
    if [[ -z "${id}" ]]; then
        id="$(grep -ohE 'bestpractices\.dev/(en/)?projects/[0-9]+' "${root}/README.md" "${root}/docs/security/openssf.md" 2>/dev/null |
            head -n 1 | grep -oE '[0-9]+$' || true)"
    fi
    if [[ -z "${id}" ]]; then
        pending "no bestpractices.dev project id in README.md or docs/security/openssf.md (the owner registers the project, 0.11.0 D16)" \
            "passing" "Best Practices"
        return
    fi
    if ((OFFLINE)); then
        pending "offline" "passing" "Best Practices"
        return
    fi
    code="$(http_get "https://www.bestpractices.dev/projects/${id}.json" "${tmp}/bestpractices.json")"
    if [[ "${code}" != 200 ]]; then
        pending "bestpractices.dev answered HTTP ${code} for project ${id}" "passing" "Best Practices"
        return
    fi
    jq --arg id "${id}" '
        (.badge_level // "unknown") as $l
        | {value: $l, target: "passing", project: $id,
           status: (if ($l == "passing" or $l == "silver" or $l == "gold") then "pass" else "fail" end),
           summary: "Best Practices \($l) (project \($id))"}' "${tmp}/bestpractices.json"
}

kpi_fuzz_open_crashes() {
    local runs=""
    if ((OFFLINE)); then
        pending "offline" "0" "fuzz open crashes"
        return
    fi
    if ! command -v gh >/dev/null; then
        pending "gh not on PATH" "0" "fuzz open crashes"
        return
    fi
    runs="$(gh run list -R "${REPO_SLUG}" --workflow cflite_batch.yml --branch main -L 1 \
        --json conclusion,status,createdAt,url,headSha 2>/dev/null || true)"
    if [[ -z "${runs}" || "$(jq 'length' <<<"${runs}")" == 0 ]]; then
        pending "cflite_batch.yml has not run on main yet (needs the release push)" "0" "fuzz open crashes"
        return
    fi
    jq '.[0] as $r
        | if $r.status != "completed" then
            {value: null, target: "0", status: "pending", reason: "the latest batch run is \($r.status)",
             run: $r.url, summary: "fuzz open crashes pending: batch run \($r.status)"}
          elif $r.conclusion == "success" then
            {value: 0, target: "0", status: "pass", run: $r.url, date: $r.createdAt,
             summary: "0 open crashes (batch run \($r.createdAt))"}
          else
            {value: null, target: "0", status: "fail", run: $r.url, date: $r.createdAt,
             summary: "the latest batch run concluded \($r.conclusion): \($r.url)"}
          end' <<<"${runs}"
}

kpi_slsa() {
    local root="$1" version="$2" tmp="$3" crates=() name code published=0 unpublished=()
    local target run_id="" verify="" assets=""
    mapfile -t crates < <(bash "${root}/scripts/validate-crate-packaging.sh" --print-order 2>/dev/null || true)
    if ((${#crates[@]} == 0)); then
        jq -n '{value: null, target: "every published crate", status: "fail",
            summary: "no publish order from scripts/validate-crate-packaging.sh --print-order"}'
        return
    fi
    target="Build L3, ${#crates[@]}/${#crates[@]} crates verified"
    if ((OFFLINE)); then
        pending "offline" "${target}" "SLSA"
        return
    fi
    code="$(http_get "https://crates.io/api/v1/crates/${crates[0]}/${version}" "${tmp}/crate.json")"
    if [[ "${code}" != 200 ]]; then
        pending "${crates[0]} ${version} is not on crates.io (needs the release publish; HTTP ${code})" "${target}" "SLSA"
        return
    fi
    for name in "${crates[@]}"; do
        sleep 1 # crates.io crawler policy: at most one request per second
        code="$(http_get "https://crates.io/api/v1/crates/${name}/${version}" "${tmp}/crate.json")"
        if [[ "${code}" == 200 ]]; then
            published=$((published + 1))
        else
            unpublished+=("${name}")
        fi
    done
    if command -v gh >/dev/null; then
        run_id="$(gh run list -R "${REPO_SLUG}" --workflow publish.yml -L 50 --json databaseId,headBranch \
            --jq "[.[] | select(.headBranch == \"v${version}\")][0].databaseId // empty" 2>/dev/null || true)"
        if [[ -n "${run_id}" ]]; then
            verify="$(gh run view "${run_id}" -R "${REPO_SLUG}" --json jobs \
                --jq ".jobs[] | select(.name == \"${VERIFY_JOB}\") | .conclusion" 2>/dev/null || true)"
        fi
        assets="$(gh release view "v${version}" -R "${REPO_SLUG}" --json assets --jq '.assets[].name' 2>/dev/null || true)"
    fi
    jq -n --argjson n "${#crates[@]}" --argjson pub "${published}" --arg verify "${verify}" \
        --arg run "${run_id}" --arg prov "id_effect-${version}.intoto.jsonl" --arg assets "${assets}" \
    --arg target "${target}" '
        $ARGS.positional as $missing
        | ($assets | split("\n") | index($prov) != null) as $has_prov
        | (if $verify == "success" and $has_prov then $pub else 0 end) as $verified
        | if $verify == "" then
            {value: null, level: "Build L3", crates: $n, published: $pub, unpublished: $missing,
             target: $target, status: "pending",
             reason: "no publish.yml verify job result for the tag (gh unavailable or the run is not found)",
             summary: "SLSA pending: \($pub)/\($n) published, verify job not found"}
          else
            {value: $verified, level: "Build L3", crates: $n, published: $pub, unpublished: $missing,
             verify_job: $verify, run: $run, provenance: $prov, provenance_in_release: $has_prov,
             target: $target, status: (if $verified == $n then "pass" else "fail" end),
             summary: "SLSA Build L3: \($verified)/\($n) crates verified (verify job \($verify))"}
          end' --args "${unpublished[@]}"
}

# --- report ----------------------------------------------------------------------------------

# Prints one line per KPI of the report JSON on stdin.
print_lines() {
    jq -r '.kpis | to_entries[]
        | "security-kpis: \(.key) \(.value.summary) (target \(.value.target)) \(if .value.status == "fail" then "FAIL" else .value.status end)"'
}

# Computes every KPI for ROOT into the directory TMP and prints the report JSON.
report() {
    local root="$1" version="$2" bench="$3" tmp="$4" adr="$1/docs/adrs/0013-secure-by-default.md" revision="" k
    local order=(open_highs adr_0013_enforcement adr_0013_status fuzz_targets fuzz_open_crashes
        compile_fail_cases secure_stack check_dangerous check_workflows sha_pinned_actions
    registry_token openssf_scorecard best_practices slsa)
    if [[ -n "${SECURITY_SPEC}" ]]; then
        order+=(security_table)
        kpi_security_table "${root}" "${SECURITY_SPEC}" "${tmp}" >"${tmp}/security_table.json"
    fi
    if [[ -n "${T12_BENCH}" ]]; then
        order+=(t12_benches)
        kpi_t12_benches "${root}" "${T12_BENCH}" >"${tmp}/t12_benches.json"
    fi
    if ((UNARY_KPI)); then
        order+=(unary_rpc)
        kpi_unary_rpc "${root}" "${bench}" >"${tmp}/unary_rpc.json"
    fi
    if ((LOCAL_FUZZ_KPI)); then
        order+=(fuzz_local_crashes)
        kpi_fuzz_local_crashes "${root}" >"${tmp}/fuzz_local_crashes.json"
    fi
    if is_git "${root}"; then
        revision="$(git -C "${root}" rev-parse --short=8 HEAD 2>/dev/null || true)"
    fi
    kpi_open_highs "${root}" "${root}/docs/security/findings.json" >"${tmp}/open_highs.json"
    kpi_adr_enforcement "${root}" "${adr}" >"${tmp}/adr_0013_enforcement.json"
    kpi_adr_status "${adr}" >"${tmp}/adr_0013_status.json"
    kpi_fuzz_targets "${root}" "${BASELINE}" >"${tmp}/fuzz_targets.json"
    kpi_fuzz_open_crashes >"${tmp}/fuzz_open_crashes.json"
    kpi_compile_fail_cases "${root}" >"${tmp}/compile_fail_cases.json"
    kpi_secure_stack "${root}" "${bench}" >"${tmp}/secure_stack.json"
    kpi_script "${root}" scripts/check-dangerous.sh >"${tmp}/check_dangerous.json"
    kpi_script "${root}" scripts/check-workflows.sh >"${tmp}/check_workflows.json"
    kpi_sha_pinned_actions "${root}" >"${tmp}/sha_pinned_actions.json"
    s3_no_registry_token_in_workflows "${root}" >"${tmp}/registry_token.json"
    kpi_scorecard "${tmp}" >"${tmp}/openssf_scorecard.json"
    kpi_best_practices "${root}" "${tmp}" >"${tmp}/best_practices.json"
    kpi_slsa "${root}" "${version}" "${tmp}" >"${tmp}/slsa.json"
    for k in "${order[@]}"; do
        jq --arg k "${k}" '{($k): .}' "${tmp}/${k}.json"
    done | jq -s --arg release "${version}" --arg revision "${revision}" \
        --arg wsv "$(workspace_version "${root}")" --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson offline "${OFFLINE}" '
        add as $kpis
        | {schema: "id_effect security-kpis v1", method: "scripts/security-kpis.sh (header)",
           release: $release, workspace_version: $wsv, measured_at: $at, revision: $revision,
           offline: ($offline == 1),
           pass: ([$kpis[] | select(.status == "fail")] | length == 0),
           failed: [$kpis | to_entries[] | select(.value.status == "fail") | .key],
           pending: [$kpis | to_entries[] | select(.value.status == "pending" or .value.status == "not measured") | .key],
           kpis: $kpis}'
}

# --- self-test -------------------------------------------------------------------------------

self_test() {
    local tmp pass=0 out t want
    tmp="$(mktemp -d)"
    trap 'rm -rf "${tmp}"' RETURN
    OFFLINE=1
    local fx="${tmp}/repo" adr_file
    adr_file="${fx}/docs/adrs/0013-secure-by-default.md"
    mkdir -p "${fx}/crates/demo/src/observability" "${fx}/crates/demo/tests" "${fx}/scripts" \
        "${fx}/tests/security_compile_fail/ui" "${fx}/fuzz/fuzz_targets" "${fx}/.github/workflows" \
        "${fx}/.clusterfuzzlite" "${fx}/docs/security" "${fx}/docs/adrs"
    cat >"${fx}/crates/demo/tests/cases.rs" <<'RS'
#[test]
fn zz1_fixture_regression_case() {}
#[test]
fn zz2_family_one() {}
RS
    : >"${fx}/crates/demo/src/observability/posture.rs"
    : >"${fx}/scripts/fixture-gate.sh"

    st_fail() {
        echo "FAIL $1: $2" >&2
        return 1
    }
    # expect CASE FILE FILTER: the JSON object in FILE satisfies the jq FILTER.
    expect() {
        if ! jq -e "$3" "$2" >/dev/null; then
            st_fail "$1" "expected $3, got $(jq -c . "$2")"
            return 1
        fi
        pass=$((pass + 1))
    }
    # A complete register: every review id closed with the fixture test.
    register() {
        jq -n '{findings: [$ARGS.positional[]
            | {id: ., severity: (if startswith("H") then "high" elif startswith("M") then "medium"
                                 elif startswith("L") then "low" else "supply-chain" end),
               status: "closed", tests: ["zz1_fixture_regression_case"], gates: [], commits: []}]}' \
            --args "${EXPECTED_IDS[@]}"
    }
    # findings JQ: the register edited by the jq filter JQ, evaluated into o.json.
    findings() {
        register | jq "$1" >"${fx}/docs/security/findings.json"
        kpi_open_highs "${fx}" "${fx}/docs/security/findings.json" >"${tmp}/o.json"
    }

    # ── kpi_open_highs_counts_open_highs ─────────────────────────────────────────────────────
    findings .
    expect kpi_open_highs_counts_open_highs "${tmp}/o.json" '.status == "pass" and .value == 0 and .findings == 41 and .closed == 41'
    findings '(.findings[] | select(.id == "H8") | .status) = "open"'
    expect kpi_open_highs_counts_open_highs "${tmp}/o.json" '.status == "fail" and .value == 1 and .open == 1'
    findings '(.findings[] | select(.id == "M3") | .status) = "open"'
    expect kpi_open_highs_counts_open_highs "${tmp}/o.json" '.status == "pass" and .value == 0 and .open == 1'

    # ── kpi_register_complete ────────────────────────────────────────────────────────────────
    findings 'del(.findings[] | select(.id == "S7")) | .findings += [.findings[0]]'
    expect kpi_register_complete "${tmp}/o.json" '.status == "fail" and (.problems | index("S7: missing from the register")) != null and (.problems | index("H1: listed twice")) != null'
    findings '(.findings[] | select(.id == "L6")) |= (.status = "accepted" | .tests = [])'
    expect kpi_register_complete "${tmp}/o.json" '.status == "fail" and .problems == ["L6: accepted without a reason (note)"]'
    findings '(.findings[] | select(.id == "L6")) |= (.status = "accepted" | .tests = [] | .note = "no fix upstream")'
    expect kpi_register_complete "${tmp}/o.json" '.status == "pass" and .accepted == 1'
    findings '(.findings[] | select(.id == "L6")) |= (.status = "wontfix")'
    expect kpi_register_complete "${tmp}/o.json" '.status == "fail" and (.problems | length) == 1 and (.problems[0] | test("^L6: unknown status .wontfix.$"))'

    # ── kpi_register_tests_exist ─────────────────────────────────────────────────────────────
    findings '(.findings[] | select(.id == "M5")) |= (.tests = ["zz9_no_such_case"] | .gates = ["scripts/no-such-gate.sh"])'
    expect kpi_register_tests_exist "${tmp}/o.json" '.status == "fail" and .problems == ["M5: regression test zz9_no_such_case not found in the code", "M5: gate scripts/no-such-gate.sh does not exist"]'
    findings '(.findings[] | select(.id == "M5")) |= (.tests = [] | .gates = [])'
    expect kpi_register_tests_exist "${tmp}/o.json" '.status == "fail" and .problems == ["M5: closed without a regression test or gate"]'
    findings '(.findings[] | select(.id == "M5")) |= (.tests = [] | .gates = ["scripts/fixture-gate.sh"])'
    expect kpi_register_tests_exist "${tmp}/o.json" '.status == "pass"'

    # ── kpi_adr_rule_enforcement_exists ──────────────────────────────────────────────────────
    # adr STATUS R3_CELL: a rules table whose R3 enforcement cell is R3_CELL.
    adr() {
        local i ok="${BT}zz1_fixture_regression_case${BT}; ${BT}zz2_*${BT}; ${BT}scripts/fixture-gate.sh${BT}; ${BT}demo::observability::posture${BT}"
        printf '# ADR\n\n## Status\n\n%s (2026-10-18).\n\n### The ten rules\n\n' "$1"
        printf '| Rule | Statement | Findings | Enforcement point (mechanical) | Leaves |\n|---|---|---|---|---|\n'
        for i in {1..10}; do
            if ((i == 3)); then
                printf '| R3 | s | f | %s | l |\n' "$2"
            else
                printf '| R%d | s | f | %s | l |\n' "${i}" "${ok}"
            fi
        done
        printf '\n### Next\n\n| R11 | not a rule row | | %s | |\n' "${BT}zz9_no_such_case${BT}"
    }
    adr Proposed "${BT}zz1_fixture_regression_case${BT}" >"${adr_file}"
    kpi_adr_enforcement "${fx}" "${adr_file}" >"${tmp}/a.json"
    expect kpi_adr_rule_enforcement_exists "${tmp}/a.json" '.status == "pass" and .value == 10 and .rules == 10'
    kpi_adr_status "${adr_file}" >"${tmp}/a.json"
    expect kpi_adr_rule_enforcement_exists "${tmp}/a.json" '.status == "pending" and .value == "Proposed"'
    adr Accepted 'a posture probe' >"${adr_file}"
    kpi_adr_status "${adr_file}" >"${tmp}/a.json"
    expect kpi_adr_rule_enforcement_exists "${tmp}/a.json" '.status == "pass"'
    kpi_adr_enforcement "${fx}" "${adr_file}" >"${tmp}/a.json"
    expect kpi_adr_rule_enforcement_exists "${tmp}/a.json" '.status == "fail" and .value == 9 and .problems == ["R3: no enforcement point"]'
    adr Accepted "${BT}zz9_no_such_case${BT}, ${BT}zz9_*${BT}, ${BT}scripts/zz9.sh${BT}, ${BT}demo::zz9${BT}" >"${adr_file}"
    kpi_adr_enforcement "${fx}" "${adr_file}" >"${tmp}/a.json"
    expect kpi_adr_rule_enforcement_exists "${tmp}/a.json" '.status == "fail" and (.problems | length) == 4'

    # ── kpi_fuzz_targets_counted ─────────────────────────────────────────────────────────────
    for t in alpha beta gamma; do
        : >"${fx}/fuzz/fuzz_targets/${t}.rs"
    done
    cat >"${fx}/fuzz/Cargo.toml" <<'TOML'
[[bin]]
name = "alpha"
path = "fuzz_targets/alpha.rs"

[[bin]]
name = "beta"
path = "fuzz_targets/beta.rs"
TOML
    cat >"${fx}/.clusterfuzzlite/build.sh" <<'SH'
cd fuzz
for target in $(cargo fuzz list); do :; done
SH
    : >"${fx}/.github/workflows/cflite_pr.yml"
    kpi_fuzz_targets "${fx}" >"${tmp}/f.json"
    expect kpi_fuzz_targets_counted "${tmp}/f.json" '.status == "fail" and .value == 2 and .problems == [".github/workflows/cflite_batch.yml missing", "not a [[bin]] of fuzz/Cargo.toml: gamma"]'
    rm "${fx}/fuzz/fuzz_targets/gamma.rs"
    : >"${fx}/.github/workflows/cflite_batch.yml"
    kpi_fuzz_targets "${fx}" >"${tmp}/f.json"
    expect kpi_fuzz_targets_counted "${tmp}/f.json" '.status == "fail" and .value == 2 and .problems == []'
    FUZZ_MIN=2 kpi_fuzz_targets "${fx}" >"${tmp}/f.json"
    expect kpi_fuzz_targets_counted "${tmp}/f.json" '.status == "pass" and .targets == ["alpha", "beta"]'

    # ── kpi_compile_fail_cases_counted ───────────────────────────────────────────────────────
    for t in one two three; do
        : >"${fx}/tests/security_compile_fail/ui/${t}.rs"
    done
    : >"${fx}/tests/security_compile_fail/ui/one.stderr"
    : >"${fx}/tests/security_compile_fail/ui/two.stderr"
    COMPILE_FAIL_MIN=2 kpi_compile_fail_cases "${fx}" >"${tmp}/c.json"
    expect kpi_compile_fail_cases_counted "${tmp}/c.json" '.status == "fail" and .value == 2 and .problems == ["three.rs has no .stderr"]'
    : >"${fx}/tests/security_compile_fail/ui/three.stderr"
    COMPILE_FAIL_MIN=3 kpi_compile_fail_cases "${fx}" >"${tmp}/c.json"
    expect kpi_compile_fail_cases_counted "${tmp}/c.json" '.status == "pass" and .value == 3'
    kpi_compile_fail_cases "${fx}" >"${tmp}/c.json"
    expect kpi_compile_fail_cases_counted "${tmp}/c.json" '.status == "fail"'

    # ── kpi_secure_stack_delta ───────────────────────────────────────────────────────────────
    # bench SECURE BARE: an export with those medians (ns).
    bench() {
        jq -n --argjson s "$1" --argjson b "$2" --arg sid "${SECURE_ID}" --arg bid "${BARE_ID}" \
            '{schema: 1, sha: "abc", benches: {($sid): {median: $s}, ($bid): {median: $b}}}'
    }
    bench 5900 900 >"${tmp}/bench.json"
    kpi_secure_stack "${fx}" "${tmp}/bench.json" >"${tmp}/s.json"
    expect kpi_secure_stack_delta "${tmp}/s.json" '.status == "pass" and .value == 5000 and .ratio == 6.56'
    bench 5901 900 >"${tmp}/bench.json"
    kpi_secure_stack "${fx}" "${tmp}/bench.json" >"${tmp}/s.json"
    expect kpi_secure_stack_delta "${tmp}/s.json" '.status == "fail" and .value == 5001'
    bench 1000 900 | jq --arg bid "${BARE_ID}" 'del(.benches[$bid])' >"${tmp}/bench.json"
    kpi_secure_stack "${fx}" "${tmp}/bench.json" >"${tmp}/s.json"
    expect kpi_secure_stack_delta "${tmp}/s.json" '.status == "not measured"'
    kpi_secure_stack "${fx}" "${tmp}/no-such-bench.json" >"${tmp}/s.json"
    expect kpi_secure_stack_delta "${tmp}/s.json" '.status == "not measured" and .value == null'

    # ── kpi_sha_pinned_actions ───────────────────────────────────────────────────────────────
    cat >"${fx}/.github/workflows/a.yml" <<'YML'
jobs:
  a:
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: "Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6" # v2.9.2
      - uses: ./.github/actions/local
      # - uses: actions/checkout@v4
      - uses: docker://alpine@sha256:4bcff63911fcb4448bd4fdacec207030997caf25e9bea4045fa6c8c44de311d1
  slsa:
    uses: slsa-framework/slsa-github-generator/.github/workflows/generator_generic_slsa3.yml@v2.1.0
YML
    kpi_sha_pinned_actions "${fx}" >"${tmp}/p.json"
    expect kpi_sha_pinned_actions "${tmp}/p.json" '.status == "pass" and .uses == 4 and .sha_pinned == 3 and .tag_pinned_exception == 1 and .value == 75'
    cat >"${fx}/.github/workflows/b.yml" <<'YML'
jobs:
  b:
    steps:
      - uses: dtolnay/rust-toolchain@stable
YML
    kpi_sha_pinned_actions "${fx}" >"${tmp}/p.json"
    expect kpi_sha_pinned_actions "${tmp}/p.json" '.status == "fail" and .uses == 5 and .sha_pinned == 3'

    # ── s3_no_registry_token_in_workflows ────────────────────────────────────────────────────
    s3_no_registry_token_in_workflows "${fx}" >"${tmp}/r.json"
    expect s3_no_registry_token_in_workflows "${tmp}/r.json" '.status == "pass" and .value == 0'
    cat >"${fx}/.github/workflows/c.yml" <<'YML'
env:
  CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
YML
    s3_no_registry_token_in_workflows "${fx}" >"${tmp}/r.json"
    expect s3_no_registry_token_in_workflows "${tmp}/r.json" '.status == "fail" and .files == [".github/workflows/c.yml"]'

    # ── kpi_fuzz_targets_new (0.12) ──────────────────────────────────────────────────────────
    jq -n '{kpis: {fuzz_targets: {targets: ["alpha"]}}}' >"${tmp}/base.json"
    FUZZ_MIN=2 FUZZ_NEW_MIN=1 kpi_fuzz_targets "${fx}" "${tmp}/base.json" >"${tmp}/f.json"
    expect kpi_fuzz_targets_new "${tmp}/f.json" '.status == "pass" and .new == ["beta"] and .new_count == 1 and .target == ">= 2 (>= 1 new)"'
    FUZZ_MIN=2 FUZZ_NEW_MIN=2 kpi_fuzz_targets "${fx}" "${tmp}/base.json" >"${tmp}/f.json"
    expect kpi_fuzz_targets_new "${tmp}/f.json" '.status == "fail" and .value == 2 and .new_count == 1'
    echo '{}' >"${tmp}/base.json"
    FUZZ_MIN=2 kpi_fuzz_targets "${fx}" "${tmp}/base.json" >"${tmp}/f.json"
    expect kpi_fuzz_targets_new "${tmp}/f.json" '.status == "fail" and (.problems[0] | test("no fuzz_targets.targets in the baseline"))'

    # ── kpi_compile_fail_required (0.12) ─────────────────────────────────────────────────────
    REQUIRED_CASES=(one three)
    COMPILE_FAIL_MIN=3 kpi_compile_fail_cases "${fx}" >"${tmp}/c.json"
    expect kpi_compile_fail_required "${tmp}/c.json" '.status == "pass" and .required == ["one", "three"]'
    REQUIRED_CASES=(one four)
    COMPILE_FAIL_MIN=3 kpi_compile_fail_cases "${fx}" >"${tmp}/c.json"
    expect kpi_compile_fail_required "${tmp}/c.json" '.status == "fail" and .problems == ["required case four missing"]'
    REQUIRED_CASES=()

    # ── kpi_security_table (0.12) ────────────────────────────────────────────────────────────
    # spec CELL: a release spec whose Security table has the rows below and one with CELL.
    spec() {
        printf '# Spec\n\n## Security\n\n| Threat | Default | ADR 0013 rule | Enforcement point (mechanical) | Leaf |\n|---|---|---|---|---|\n'
        printf '| A / B | d | R1 | compile-fail %sthree%s; runtime %szz1_fixture_regression_case%s | l |\n' "${BT}" "${BT}" "${BT}" "${BT}"
        printf '| C | d | R9 | %sdangerous_zz_opt_out%s recorded; posture probe "x"; %s/rpc/x%s closed | l |\n' "${BT}" "${BT}" "${BT}" "${BT}"
        printf '| D | d | R4 | %s | l |\n\n## Epics\n\n| E | d | R | %szz9_no_such_case%s | l |\n' "$1" "${BT}" "${BT}"
    }
    printf 'pub fn dangerous_zz_opt_out() {}\n' >"${fx}/crates/demo/src/opt.rs"
    # The stand-ins for cargo: LISTED is what `nextest list` prints, RUN_OK and CASES_OK the exits;
    # each call is logged to calls (table_list runs in a process substitution).
    local LISTED=(demo::zz2_family_one) RUN_OK=0 CASES_OK=0
    : >"${tmp}/calls"
    table_list() {
        echo "list:$3" >>"${tmp}/calls"
        printf '%s\n' "${LISTED[@]}"
    }
    table_run() {
        echo run >>"${tmp}/calls"
        if ((RUN_OK)); then return 0; fi
        printf '        FAIL [   0.004s] demo::cases zz2_family_one\n' >"$2"
        return 100
    }
    cases_run() {
        echo cases >>"${tmp}/calls"
        ((CASES_OK))
    }
    spec "${BT}zz2_family_one${BT}" >"${tmp}/spec.md"
    RUN_TABLE_TESTS=0 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table "${tmp}/t.json" '.status == "not measured" and .names == 4 and .tests == ["zz1_fixture_regression_case", "zz2_family_one"] and .compile_fail_cases == ["three"] and .other_points == ["dangerous_zz_opt_out"] and .problems == []'
    [[ ! -s "${tmp}/calls" ]] || st_fail kpi_security_table "--no-run-tests still ran cargo: $(<"${tmp}/calls")" || return 1
    RUN_TABLE_TESTS=1 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table "${tmp}/t.json" '.status == "fail" and .run == "failed" and .problems == ["zz1_fixture_regression_case: not a test of the --all-features workspace build (cargo nextest list)", "the Security-table tests failed: zz2_family_one", "the compile-fail suite (cargo nextest run -p security_compile_fail) failed"]'
    [[ "$(<"${tmp}/calls")" == $'list:test(/(^|::)(zz1_fixture_regression_case|zz2_family_one)$/)\nrun\ncases' ]] ||
    st_fail kpi_security_table "cargo calls: $(<"${tmp}/calls")" || return 1
    LISTED=(demo::cases::zz1_fixture_regression_case zz2_family_one)
    RUN_OK=1 CASES_OK=1
    RUN_TABLE_TESTS=1 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table "${tmp}/t.json" '.status == "pass" and .value == "green" and .listed == 2'
    : >"${tmp}/calls"
    spec "${BT}zz9_no_such_case${BT}" >"${tmp}/spec.md"
    RUN_TABLE_TESTS=1 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table "${tmp}/t.json" '.status == "fail" and .problems == ["D: zz9_no_such_case not found in the code"]'
    [[ ! -s "${tmp}/calls" ]] || st_fail kpi_security_table "ran cargo although a name is missing: $(<"${tmp}/calls")" || return 1
    printf '# Spec\n\n## Security\n\nNo table.\n' >"${tmp}/spec.md"
    kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table "${tmp}/t.json" '.status == "fail" and .names == 0'

    # ── kpi_t12_benches (0.12) ───────────────────────────────────────────────────────────────
    t12() {
        jq -n '{sha: "abc", created: "t",
            gates_0_12: {unary_delta_ns: 1776.3, unary_ratio: 2.87, unary_before_ns: 3399.3,
                         batch_over_sequential: 0.111, h3_over_h2_p99: 0.065, h3_mode: "proxy"},
            soak: {streams: 10000, duration_s: 600, heartbeats_received: 6009790, heartbeats_missing: 0,
                   heartbeats_out_of_order: 0, streams_short: 0, streams_ended_early: 0,
                   rss_kib: {mark_s: 60, at_mark: 341648, end: 355280, peak: 357176, growth: 1.0399,
                             samples: [[60, 341648], [299.9, 356000], [300.0004, 356240], [600, 355280]]}}}' | jq "$1"
    }
    t12 . >"${tmp}/b.json"
    kpi_t12_benches "${fx}" "${tmp}/b.json" >"${tmp}/k.json"
    expect kpi_t12_benches "${tmp}/k.json" '.status == "pass" and .value == 4 and .unary_delta_ns.value == 1776 and .batch_over_sequential.speedup == 9 and .soak.rss_growth == 0.997 and .soak.rss_growth_minute_1 == 1.04 and .soak.rss_at_300s_kib == 356240 and .soak.rss_growth_after_300s == 0.997'
    # a run marked at minute 5 by the example (F60): its own growth is the gate
    t12 '.soak.rss_kib = {mark_s: 300, at_mark: 330000, end: 349800, peak: 350000, growth: 1.06, at_minute_1: 320000,
         samples: [[60, 320000], [300, 330000], [600, 349800]]}' >"${tmp}/b.json"
    kpi_t12_benches "${fx}" "${tmp}/b.json" >"${tmp}/k.json"
    expect kpi_t12_benches "${tmp}/k.json" '.status == "fail" and .failed == ["soak"] and .soak.rss_growth == 1.06 and .soak.rss_growth_minute_1 == 1.093'
    t12 '.soak.heartbeats_missing = 1 | .gates_0_12.batch_over_sequential = 0.21' >"${tmp}/b.json"
    kpi_t12_benches "${fx}" "${tmp}/b.json" >"${tmp}/k.json"
    expect kpi_t12_benches "${tmp}/k.json" '.status == "fail" and .value == 2 and .failed == ["batch_over_sequential", "soak"]'
    t12 'del(.gates_0_12.unary_delta_ns) | del(.soak.rss_kib.samples) | .gates_0_12.h3_over_h2_p99 = 1.01' >"${tmp}/b.json"
    kpi_t12_benches "${fx}" "${tmp}/b.json" >"${tmp}/k.json"
    expect kpi_t12_benches "${tmp}/k.json" '.status == "fail" and .value == 1 and .failed == ["unary_delta_ns", "h3_over_h2_p99", "soak"]'
    kpi_t12_benches "${fx}" "${tmp}/no-such.json" >"${tmp}/k.json"
    expect kpi_t12_benches "${tmp}/k.json" '.status == "fail" and .value == null'

    # ── kpi_unary_rpc_delta (0.13) ───────────────────────────────────────────────────────────
    # unary UNARY HANDLER: an export with those medians (ns).
    unary() {
        jq -n --argjson u "$1" --argjson h "$2" --arg uid "${UNARY_ID}" --arg hid "${HANDLER_ID}" \
            '{schema: 1, sha: "abc", benches: {($uid): {median: $u}, ($hid): {median: $h}}}'
    }
    unary 3969.4 969.4 >"${tmp}/bench.json"
    kpi_unary_rpc "${fx}" "${tmp}/bench.json" >"${tmp}/u.json"
    expect kpi_unary_rpc_delta "${tmp}/u.json" '.status == "pass" and .value == 3000 and .target == "<= 3000 ns" and .unary_json_ns == 3969.4 and .axum_json_handler_ns == 969.4 and .ratio == 4.09'
    unary 3970.6 969.4 >"${tmp}/bench.json"
    kpi_unary_rpc "${fx}" "${tmp}/bench.json" >"${tmp}/u.json"
    expect kpi_unary_rpc_delta "${tmp}/u.json" '.status == "fail" and .value == 3001'
    bench 1000 900 >"${tmp}/bench.json"
    kpi_unary_rpc "${fx}" "${tmp}/bench.json" >"${tmp}/u.json"
    expect kpi_unary_rpc_delta "${tmp}/u.json" '.status == "not measured" and (.reason | test("rpc/unary_json"))'
    kpi_unary_rpc "${fx}" "${tmp}/no-such-bench.json" >"${tmp}/u.json"
    expect kpi_unary_rpc_delta "${tmp}/u.json" '.status == "not measured" and (.reason | test("id_effect_rpc:gate_rpc"))'
    # the secure stack keeps its own medians beside the shared fields
    bench 3621 923 >"${tmp}/bench.json"
    kpi_secure_stack "${fx}" "${tmp}/bench.json" >"${tmp}/s.json"
    expect kpi_unary_rpc_delta "${tmp}/s.json" '.value == 2698 and .secure_stack_ns == 3621 and .bare_ns == 923 and .bench == "gate/axum_request_secure_stack" and .stale == false'

    # ── kpi_fuzz_local_crashes (0.13) ────────────────────────────────────────────────────────
    kpi_fuzz_local_crashes "${fx}" >"${tmp}/l.json"
    expect kpi_fuzz_local_crashes "${tmp}/l.json" '.status == "not measured" and .value == null'
    mkdir -p "${fx}/fuzz/artifacts/alpha" "${fx}/fuzz/artifacts/beta"
    kpi_fuzz_local_crashes "${fx}" >"${tmp}/l.json"
    expect kpi_fuzz_local_crashes "${tmp}/l.json" '.status == "pass" and .value == 0 and .targets_run == ["alpha", "beta"]'
    : >"${fx}/fuzz/artifacts/beta/crash-0123abcd"
    : >"${fx}/fuzz/artifacts/beta/oom-4567"
    : >"${fx}/fuzz/artifacts/alpha/notes.txt"
    kpi_fuzz_local_crashes "${fx}" >"${tmp}/l.json"
    expect kpi_fuzz_local_crashes "${tmp}/l.json" '.status == "fail" and .value == 2 and .inputs == ["beta/crash-0123abcd", "beta/oom-4567"]'
    rm -rf "${fx}/fuzz/artifacts"

    # ── kpi_security_table_dangerous_tests (0.13) ────────────────────────────────────────────
    # A dangerous_* name with a test attribute above its fn is a test; the opt-out stays a point.
    printf '#[tokio::test(flavor = "current_thread")]\nasync fn dangerous_zz_opt_out_is_recorded() {}\n' \
        >"${fx}/crates/demo/tests/opt_out.rs"
    spec "${BT}dangerous_zz_opt_out_is_recorded${BT}" >"${tmp}/spec.md"
    : >"${tmp}/calls"
    RUN_TABLE_TESTS=0 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table_dangerous_tests "${tmp}/t.json" '.status == "not measured" and .tests == ["zz1_fixture_regression_case", "dangerous_zz_opt_out_is_recorded"] and .other_points == ["dangerous_zz_opt_out"]'
    LISTED=(demo::zz1_fixture_regression_case opt_out::dangerous_zz_opt_out_is_recorded)
    RUN_TABLE_TESTS=1 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table_dangerous_tests "${tmp}/t.json" '.status == "pass" and .listed == 2'
    [[ "$(head -n 1 "${tmp}/calls")" == 'list:test(/(^|::)(zz1_fixture_regression_case|dangerous_zz_opt_out_is_recorded)$/)' ]] ||
    st_fail kpi_security_table_dangerous_tests "cargo calls: $(<"${tmp}/calls")" || return 1
    # a dangerous_* fn under a non-test attribute is still an opt-out
    printf '#[must_use]\npub fn dangerous_zz_opt_out_is_recorded() {}\n' >"${fx}/crates/demo/tests/opt_out.rs"
    RUN_TABLE_TESTS=0 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table_dangerous_tests "${tmp}/t.json" '.tests == ["zz1_fixture_regression_case"] and .other_points == ["dangerous_zz_opt_out", "dangerous_zz_opt_out_is_recorded"]'
    rm "${fx}/crates/demo/tests/opt_out.rs"

    # ── kpi_security_table_fuzz_targets (0.13) ───────────────────────────────────────────────
    # A name with a fuzz/fuzz_targets/<name>.rs is a fuzz target (alpha and beta are [[bin]]s).
    spec "fuzz ${BT}alpha${BT}, ${BT}beta${BT}" >"${tmp}/spec.md"
    RUN_TABLE_TESTS=0 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table_fuzz_targets "${tmp}/t.json" '.status == "not measured" and .names == 5 and .fuzz_targets == ["alpha", "beta"] and .tests == ["zz1_fixture_regression_case"] and (.summary | test("2 fuzz targets"))'
    : >"${fx}/fuzz/fuzz_targets/delta.rs"
    spec "fuzz ${BT}delta${BT}" >"${tmp}/spec.md"
    RUN_TABLE_TESTS=0 kpi_security_table "${fx}" "${tmp}/spec.md" "${tmp}" >"${tmp}/t.json"
    expect kpi_security_table_fuzz_targets "${tmp}/t.json" '.status == "fail" and .problems == ["D: fuzz target delta is not a [[bin]] of fuzz/Cargo.toml"]'
    rm "${fx}/fuzz/fuzz_targets/delta.rs"

    # ── kpi_release_0_13_targets ─────────────────────────────────────────────────────────────
    jq -n '{kpis: {compile_fail_cases: {value: 19}}}' >"${fx}/docs/security/kpis-0.12.json"
    out="$(configure_release "${fx}" 0.13
        echo "${FUZZ_MIN} ${FUZZ_NEW_MIN} ${COMPILE_FAIL_MIN} ${REQUIRED_CASES[*]} ${UNARY_KPI} ${LOCAL_FUZZ_KPI} ${BASELINE#"${fx}"/} ${RELEASE_BENCH#"${fx}"/} ${SECURITY_SPEC#"${fx}"/}")"
    want="22 5 21 r1_admin_site_without_guard r1_mcp_endpoint_without_access_decision 1 1 docs/security/kpis-0.12.json docs/benchmarks/v0.13.json .maestro/specs/releases/0.13.0.md"
    [[ "${out}" == "${want}" ]] || st_fail kpi_release_0_13_targets "want '${want}', got '${out}'" || return 1
    pass=$((pass + 1))
    rm "${fx}/docs/security/kpis-0.12.json"
    if (configure_release "${fx}" 0.13) 2>/dev/null; then
        st_fail kpi_release_0_13_targets "0.13 configured without its 0.12 baseline" || return 1
    fi
    pass=$((pass + 1))

    # ── kpi_online_pending_offline ───────────────────────────────────────────────────────────
    kpi_scorecard "${tmp}" >"${tmp}/n.json"
    expect kpi_online_pending_offline "${tmp}/n.json" '.status == "pending" and .reason == "offline" and .value == null'
    kpi_best_practices "${fx}" "${tmp}" >"${tmp}/n.json"
    expect kpi_online_pending_offline "${tmp}/n.json" '.status == "pending" and (.reason | test("D16"))'
    kpi_fuzz_open_crashes >"${tmp}/n.json"
    expect kpi_online_pending_offline "${tmp}/n.json" '.status == "pending"'
    printf 'echo demo\n' >"${fx}/scripts/validate-crate-packaging.sh"
    kpi_slsa "${fx}" 0.0.1 "${tmp}" >"${tmp}/n.json"
    expect kpi_online_pending_offline "${tmp}/n.json" '.status == "pending" and .reason == "offline" and .target == "Build L3, 1/1 crates verified"'
    # pending and not measured print as such, never as FAIL.
    jq -n '{kpis: {a: {status: "pass", summary: "x", target: 1}, b: {status: "pending", summary: "y", target: 2},
                   c: {status: "not measured", summary: "z", target: 3}, d: {status: "fail", summary: "w", target: 4}}}' >"${tmp}/rep.json"
    out="$(print_lines <"${tmp}/rep.json")"
    if [[ "${out}" != $'security-kpis: a x (target 1) pass\nsecurity-kpis: b y (target 2) pending\nsecurity-kpis: c z (target 3) not measured\nsecurity-kpis: d w (target 4) FAIL' ]]; then
        st_fail kpi_online_pending_offline "report lines: ${out}"
        return 1
    fi
    pass=$((pass + 1))

    echo "security-kpis: self-test passed (${pass} checks: kpi_open_highs_counts_open_highs, kpi_register_complete, kpi_register_tests_exist, kpi_adr_rule_enforcement_exists, kpi_fuzz_targets_counted, kpi_compile_fail_cases_counted, kpi_secure_stack_delta, kpi_sha_pinned_actions, s3_no_registry_token_in_workflows, kpi_fuzz_targets_new, kpi_compile_fail_required, kpi_security_table, kpi_t12_benches, kpi_unary_rpc_delta, kpi_fuzz_local_crashes, kpi_security_table_dangerous_tests, kpi_security_table_fuzz_targets, kpi_release_0_13_targets, kpi_online_pending_offline)"
}

# --- main ------------------------------------------------------------------------------------

for tool in jq git awk grep find; do
    command -v "${tool}" >/dev/null || die "missing ${tool}"
done

version="" out=""
while (($#)); do
    case "$1" in
        --self-test)
            self_test
            exit 0
            ;;
        --out)
            out="${2:?--out needs a file}"
            shift 2
            ;;
        --offline)
            OFFLINE=1
            shift
            ;;
        --no-run-tests)
            RUN_TABLE_TESTS=0
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*) die "unknown argument: $1 (see --help)" ;;
        *)
            [[ -z "${version}" ]] || die "one VERSION only"
            version="$1"
            shift
            ;;
    esac
done
version="${version:-$(workspace_version "${ROOT}")}"
[[ "${version}" =~ ^([0-9]+)\.([0-9]+)\.[0-9]+$ ]] || die "VERSION must be X.Y.Z, got '${version}'"
out="${out:-${ROOT}/docs/security/kpis-${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.json}"
[[ "${OFFLINE}" == 1 ]] || OFFLINE=0
[[ "${RUN_TABLE_TESTS}" == 0 ]] || RUN_TABLE_TESTS=1
configure_release "${ROOT}" "${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"
bench="${SECURITY_KPIS_BENCH:-${RELEASE_BENCH:-${ROOT}/.tmp/bench-head.json}}"
if [[ -z "${SECURITY_KPIS_BENCH:-}" && ! -f "${bench}" && -n "${T12_BENCH}" ]]; then
    bench="${T12_BENCH}"
fi
bench="$(realpath -m "${bench}")"

work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
report "${ROOT}" "${version}" "${bench}" "${work}" >"${work}/report.json"
mkdir -p "$(dirname "${out}")"
cp "${work}/report.json" "${out}"
if command -v biome >/dev/null; then
    biome format --write "${out}" >/dev/null 2>&1 || echo "security-kpis: warning: biome format failed on ${out}" >&2
fi
print_lines <"${out}"
echo "security-kpis: wrote ${out#"${ROOT}"/}"
if [[ "$(jq -r '.pass' "${out}")" != true ]]; then
    echo "security-kpis: FAIL: $(jq -r '.failed | join(", ")' "${out}")" >&2
    exit 1
fi
