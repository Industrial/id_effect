#!/usr/bin/env bash
# Compute the security KPIs of a release (0.11.0.md, security_kpis and the exit gate "Security
# KPIs") and record them in docs/security/kpis-<major>.<minor>.json.
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
#                        (target >= 8).
#   compile_fail_cases   tests/security_compile_fail/ui/*.rs with a .stderr next to them
#                        (target >= 15).
#   secure_stack         gate/axum_request_secure_stack - gate/axum_request_bare (median, ns) from
#                        the bench export SECURITY_KPIS_BENCH (default .tmp/bench-head.json, written
#                        by scripts/bench-run.sh + scripts/bench-export.sh); target <= 5000 ns, the
#                        owner decision of 2026-10-19 (b66751f0; was ratio <= 1.15). The ratio is
#                        recorded for information. No export, or one without both ids: "not
#                        measured" (reported, not a FAIL).
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
# Usage: bash scripts/security-kpis.sh [--out FILE] [--offline] [VERSION]
#        bash scripts/security-kpis.sh --self-test
#   VERSION     the release, X.Y.Z (default: the workspace version in Cargo.toml)
#   --out FILE  where to write the JSON (default docs/security/kpis-<X.Y>.json)
#   --offline   do not query GitHub or crates.io: every online KPI is pending
#               (also SECURITY_KPIS_OFFLINE=1)
#   --self-test runs the kpi_* and s3_* cases below against fixtures in a temporary directory
#               (offline, no network).
# Environment: SECURITY_KPIS_BENCH (bench export), SECURITY_KPIS_REPO (default
# Industrial/id_effect), SECURITY_KPIS_BESTPRACTICES_ID.
# Output: one `security-kpis: <kpi> <summary> (target ...) pass|FAIL|pending|not measured` line
# per KPI, then the JSON file (formatted with biome when it is on PATH). Exit status: 0 when no
# KPI FAILed (pending and not measured do not fail), 1 on a FAIL, 2 on misuse.
# Requires: bash >= 4.4, jq, git, awk, grep, find; curl and gh for the online KPIs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
REPO_SLUG="${SECURITY_KPIS_REPO:-Industrial/id_effect}"
OFFLINE="${SECURITY_KPIS_OFFLINE:-0}"
UA="id_effect security-kpis (https://github.com/Industrial/id_effect)"
DELTA_LIMIT_NS=5000
FUZZ_MIN=8
COMPILE_FAIL_MIN=15
SCORECARD_MIN=7.5
SECURE_ID="gate/axum_request_secure_stack"
BARE_ID="gate/axum_request_bare"
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

kpi_fuzz_targets() {
    local root="$1" f name listed=() unlisted=() problems=()
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
    jq -n --argjson min "${FUZZ_MIN}" --arg listed "${listed[*]}" '
        $ARGS.positional as $p
        | ($listed | split(" ") | map(select(. != ""))) as $t
        | {value: ($t | length), target: ">= \($min)", targets: $t, problems: $p,
           status: (if ($t | length) >= $min and ($p | length) == 0 then "pass" else "fail" end),
           summary: "\($t | length) cargo-fuzz targets under ClusterFuzzLite"}' --args "${problems[@]}"
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
    jq -n --argjson v "${n}" --argjson min "${COMPILE_FAIL_MIN}" '
        $ARGS.positional as $p
        | {value: $v, target: ">= \($min)", problems: $p,
           status: (if $v >= $min and ($p | length) == 0 then "pass" else "fail" end),
           summary: "\($v) compile-fail cases in tests/security_compile_fail/ui"}' --args "${problems[@]}"
}

# The secure-stack overhead from the bench export BENCH (a path; shown relative to ROOT).
kpi_secure_stack() {
    local root="$1" bench="$2" sha stale=false
    if [[ ! -f "${bench}" ]]; then
        jq -n --arg b "${bench#"${root}"/}" --argjson limit "${DELTA_LIMIT_NS}" '
            {value: null, target: "<= \($limit) ns", status: "not measured", source: $b,
             reason: "no bench export (scripts/bench-run.sh --only id_effect_axum:gate_request, then scripts/bench-export.sh head \($b))",
             summary: "secure stack - bare not measured"}'
        return
    fi
    # Stale: the crates or the lock file changed between the benched commit and HEAD.
    sha="$(jq -r '.sha // empty' "${bench}" 2>/dev/null || true)"
    if is_git "${root}" && [[ -n "${sha}" ]] &&
    ! git -C "${root}" diff --quiet "${sha}" HEAD -- crates Cargo.toml Cargo.lock 2>/dev/null; then
        stale=true
    fi
    jq --arg s "${SECURE_ID}" --arg b "${BARE_ID}" --argjson limit "${DELTA_LIMIT_NS}" \
    --arg src "${bench#"${root}"/}" --argjson stale "${stale}" '
        .benches[$s].median as $sv
        | .benches[$b].median as $bv
        | if ($sv == null or $bv == null) then
            {value: null, target: "<= \($limit) ns", status: "not measured", source: $src,
             reason: "the export has no \($s) or \($b)", summary: "secure stack - bare not measured"}
          else
            (($sv - $bv) | round) as $d
            | (($sv / $bv * 100 | round) / 100) as $ratio
            | {value: $d, unit: "ns", target: "<= \($limit) ns",
               status: (if $d <= $limit then "pass" else "fail" end),
               secure_stack_ns: (($sv * 10 | round) / 10), bare_ns: (($bv * 10 | round) / 10),
               ratio: $ratio,
               ratio_note: "informational; the gate is the absolute budget (owner decision 2026-10-19, b66751f0; was <= 1.15x)",
               metric: "median", source: $src, sha: .sha, created: .created, machine: .machine,
               stale: $stale,
               summary: "+\($d) ns secure stack over bare (\($bv | round) -> \($sv | round) ns, ratio \($ratio))"}
          end' "${bench}"
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
    if is_git "${root}"; then
        revision="$(git -C "${root}" rev-parse --short=8 HEAD 2>/dev/null || true)"
    fi
    kpi_open_highs "${root}" "${root}/docs/security/findings.json" >"${tmp}/open_highs.json"
    kpi_adr_enforcement "${root}" "${adr}" >"${tmp}/adr_0013_enforcement.json"
    kpi_adr_status "${adr}" >"${tmp}/adr_0013_status.json"
    kpi_fuzz_targets "${root}" >"${tmp}/fuzz_targets.json"
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
    local tmp pass=0 out t
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

    echo "security-kpis: self-test passed (${pass} checks: kpi_open_highs_counts_open_highs, kpi_register_complete, kpi_register_tests_exist, kpi_adr_rule_enforcement_exists, kpi_fuzz_targets_counted, kpi_compile_fail_cases_counted, kpi_secure_stack_delta, kpi_sha_pinned_actions, s3_no_registry_token_in_workflows, kpi_online_pending_offline)"
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
bench="$(realpath -m "${SECURITY_KPIS_BENCH:-${ROOT}/.tmp/bench-head.json}")"

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
