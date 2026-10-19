#!/usr/bin/env bash
# Compare two bench exports against the D9 gates (docs/benchmarks/gates.toml).
#
# Usage: bash scripts/bench-compare.sh [--gates FILE] [--enforce KINDS] BASE.json NEW.json
#        bash scripts/bench-compare.sh --self-test
# BASE/NEW are written by scripts/bench-export.sh. ratio = NEW / BASE on the gates' `metric`;
# a `pair` gate's ratio is NEW[id] / NEW[over], within the new run (BASE is not read for it).
# --enforce KINDS  comma list of gate kinds (budget, improve, pair) whose FAIL counts; a missed
#                  limit of another kind prints IGNORED (CI: base and head of one change, where
#                  `improve` gates against v0.6 cannot hold). A missing id always FAILs.
# Prints one row per gate, then report-only rows for unlisted ids present in both files.
# Exits 1 on any FAIL (limit missed by more than `noise`, or a listed id missing), 2 on misuse.
# Requires: jq, awk.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATES="$ROOT/docs/benchmarks/gates.toml"
FIXTURES="$ROOT/docs/benchmarks/fixtures"

usage() { sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }

for tool in jq awk; do
    command -v "$tool" >/dev/null || { echo "bench-compare: missing $tool" >&2; exit 2; }
done

# Flatten the gates.toml subset to "metric\t<m>", "noise\t<n>" and
# "gate\t<id>\t<kind>\t<limit>\t<noise>\t<over>" (a gate's own `noise` overrides the global one;
# `over` is a pair gate's denominator id; - = unset).
parse_gates() {
    awk '
        function val(s) { sub(/^[^=]*=[[:space:]]*/, "", s); sub(/[[:space:]]*#.*$/, "", s); gsub(/"/, "", s); return s }
        function dash(s) { return s == "" ? "-" : s }
        function flush() { if (id != "") print "gate\t" id "\t" kind "\t" dash(limit) "\t" dash(gnoise) "\t" dash(over); id = kind = limit = gnoise = over = "" }
        /^[[:space:]]*(#|$)/ { next }
        /^\[\[gate\]\]/ { flush(); in_gate = 1; next }
        /^[[:space:]]*id[[:space:]]*=/ && in_gate { id = val($0); next }
        /^[[:space:]]*kind[[:space:]]*=/ && in_gate { kind = val($0); next }
        /^[[:space:]]*limit[[:space:]]*=/ && in_gate { limit = val($0); next }
        /^[[:space:]]*noise[[:space:]]*=/ && in_gate { gnoise = val($0); next }
        /^[[:space:]]*over[[:space:]]*=/ && in_gate { over = val($0); next }
        /^[[:space:]]*metric[[:space:]]*=/ && !in_gate { print "metric\t" val($0); next }
        /^[[:space:]]*noise[[:space:]]*=/ && !in_gate { print "noise\t" val($0); next }
        { printf "bench-compare: %s:%d: unsupported line: %s\n", FILENAME, NR, $0 > "/dev/stderr"; bad = 1 }
        END { flush(); exit bad }
    ' "$1"
}

# "<id>\t<value>" for every bench in an export.
values() {
    jq -r --arg m "$2" '.benches | to_entries[] | [.key, (.value[$m] // "null" | tostring)] | @tsv' "$1"
}

compare() {
    local gates="$1" base="$2" new="$3" enforce="${4:-}" spec metric kind kinds=()
    for f in "$gates" "$base" "$new"; do
        [[ -f "$f" ]] || { echo "bench-compare: no such file: $f" >&2; exit 2; }
    done
    [[ -n "$enforce" ]] && IFS=, read -ra kinds <<<"$enforce"
    for kind in "${kinds[@]}"; do
        [[ "$kind" =~ ^(budget|improve|pair)$ ]] || { echo "bench-compare: --enforce: unknown kind: $kind" >&2; exit 2; }
    done
    spec="$(parse_gates "$gates")" || exit 2
    metric="$(awk -F'\t' '$1 == "metric" { print $2 }' <<<"$spec")"
    metric="${metric:-median}"

    echo "bench-compare: $(jq -r '.baseline + "@" + (.sha // "?")[0:12]' "$base") -> $(jq -r '.baseline + "@" + (.sha // "?")[0:12]' "$new") ($metric${enforce:+; enforcing $enforce})"
    awk -F'\t' -v enforce="$enforce" '
        BEGIN { k = split(enforce, ek, ","); for (j = 1; j <= k; j++) enforced[ek[j]] = 1 }
        FILENAME == ARGV[1] {
            if ($1 == "noise") noise = $2 + 0
            if ($1 == "gate") { n++; ids[n] = $2; kinds[n] = $3; limits[n] = $4; noises[n] = $5; overs[n] = $6; listed[$2] = 1 }
            next
        }
        FILENAME == ARGV[2] { base[$1] = $2; order[++m] = $1; next }
        { cur[$1] = $2 }
        function num(v) { return v != "" && v != "null" && v + 0 > 0 }
        function row(id, kind, limit, ratio, result) {
            printf "%-38s %-8s %-6s %-7s %s\n", id, kind, limit, ratio, result
            if (result ~ /^FAIL/) fails++
        }
        END {
            printf "%-38s %-8s %-6s %-7s %s\n", "ID", "KIND", "LIMIT", "RATIO", "RESULT"
            for (i = 1; i <= n; i++) {
                id = ids[i]; kind = kinds[i]; limit = limits[i]; over = overs[i]
                if (kind == "pair") {
                    # Intra-run ratio: both ids from the new export only.
                    if (over == "-" || over == "") { row(id, kind, limit, "-", "FAIL (bad gate: pair needs over)"); continue }
                    if (!num(cur[id]) || !num(cur[over])) {
                        row(id, kind, limit, "-", "FAIL (missing in new: " (!num(cur[id]) ? id : over) ")")
                        continue
                    }
                    r = cur[id] / cur[over]
                } else {
                    if (!num(base[id]) || !num(cur[id])) {
                        row(id, kind, limit, "-", "FAIL (missing in " (!num(base[id]) ? "base" : "new") ")")
                        continue
                    }
                    r = cur[id] / base[id]
                }
                rs = sprintf("%.3f", r)
                if (kind == "report") { row(id, kind, limit, rs, "REPORT"); continue }
                if (kind != "budget" && kind != "improve" && kind != "pair" || limit == "-") {
                    row(id, kind, limit, rs, "FAIL (bad gate)"); continue
                }
                band = (noises[i] != "" && noises[i] != "-") ? noises[i] + 0 : noise
                ok = kind == "improve" ? r < limit + 0 : r <= limit + 0
                if (ok) row(id, kind, limit, rs, "PASS")
                else if (band > 0 && r <= limit + band) row(id, kind, limit, rs, "NOISE (within " band ")")
                else if (enforce != "" && !(kind in enforced)) row(id, kind, limit, rs, "IGNORED (" kind " not enforced)")
                else row(id, kind, limit, rs, "FAIL")
            }
            for (j = 1; j <= m; j++) {
                id = order[j]
                if (!(id in listed) && num(base[id]) && num(cur[id]))
                    row(id, "report", "-", sprintf("%.3f", cur[id] / base[id]), "REPORT")
            }
            printf "\nbench-compare: %d gate(s) FAIL\n", fails
            exit (fails > 0)
        }
    ' <(printf '%s\n' "$spec") <(values "$base" "$metric") <(values "$new" "$metric")
}

self_test() {
    local failed=0
    # name, new fixture, expected exit code, ids expected to FAIL (- for none),
    # --enforce kinds (- for all), ids expected IGNORED (- for none)
    while read -r name file want fail_id enforce ignore_id; do
        [[ "$enforce" == - ]] && enforce=""
        set +e
        out="$(compare "$GATES" "$FIXTURES/base.json" "$FIXTURES/$file" "$enforce" 2>&1)"
        got=$?
        set -e
        fail_rows="$(awk '$5 == "FAIL" { print $1 }' <<<"$out" | paste -sd, -)"
        ignored_rows="$(awk '$5 == "IGNORED" { print $1 }' <<<"$out" | paste -sd, -)"
        if [[ "$got" == "$want" && "${fail_rows:--}" == "$fail_id" && "${ignored_rows:--}" == "$ignore_id" ]]; then
            echo "self-test: ok   $name (exit $got)"
        else
            echo "self-test: FAIL $name (exit $got, want $want; failed ${fail_rows:--}, want $fail_id; ignored ${ignored_rows:--}, want $ignore_id)"
            printf '%s\n' "$out" | sed 's/^/    /'
            failed=1
        fi
    done <<'EOF'
identity base.json 1 gate/spawn_join_10k,gate/sleep_cpu_1k_x_1ms,gate/axum_request_oneshot - -
pass pass.json 0 - - -
fail-budget fail-budget.json 1 gate/async_bind_64 - -
fail-improve fail-improve.json 1 gate/axum_request_oneshot - -
fail-missing fail-missing.json 1 gate/sleep_cpu_1k_x_1ms - -
fail-pair fail-pair.json 1 gate/axum_request_secure_stack - -
fail-pair-missing fail-pair-missing.json 1 gate/axum_request_secure_stack - -
ci-identity base.json 0 - budget,pair gate/spawn_join_10k,gate/sleep_cpu_1k_x_1ms,gate/axum_request_oneshot
ci-fail-budget fail-budget.json 1 gate/async_bind_64 budget,pair -
ci-fail-pair fail-pair.json 1 gate/axum_request_secure_stack budget,pair -
ci-fail-missing fail-missing.json 1 gate/sleep_cpu_1k_x_1ms budget,pair -
EOF
    # Misuse: an unknown kind for --enforce exits 2.
    set +e
    (compare "$GATES" "$FIXTURES/base.json" "$FIXTURES/pass.json" budget,bogus >/dev/null 2>&1)
    got=$?
    set -e
    if [[ "$got" == 2 ]]; then
        echo "self-test: ok   enforce-misuse (exit 2)"
    else
        echo "self-test: FAIL enforce-misuse (exit $got, want 2)"
        failed=1
    fi
    ((failed == 0)) || { echo "bench-compare: self-test FAILED"; exit 1; }
    echo "bench-compare: self-test passed"
}

gates_arg="" enforce_arg=""
args=()
while (($#)); do
    case "$1" in
        --self-test) self_test; exit 0 ;;
        --gates) gates_arg="${2:?--gates needs a file}"; shift 2 ;;
        --enforce) enforce_arg="${2:?--enforce needs a list of kinds}"; shift 2 ;;
        -h | --help) usage ;;
        *) args+=("$1"); shift ;;
    esac
done
((${#args[@]} == 2)) || usage
compare "${gates_arg:-$GATES}" "${args[0]}" "${args[1]}" "$enforce_arg"
