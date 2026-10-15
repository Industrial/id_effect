#!/usr/bin/env bash
# Compare two bench exports against the D9 gates (docs/benchmarks/gates.toml).
#
# Usage: bash scripts/bench-compare.sh [--gates FILE] BASE.json NEW.json
#        bash scripts/bench-compare.sh --self-test
# BASE/NEW are written by scripts/bench-export.sh. ratio = NEW / BASE on the gates' `metric`.
# Prints one row per gate, then report-only rows for unlisted ids present in both files.
# Exits 1 on any FAIL (limit missed by more than `noise`, or a listed id missing), 2 on misuse.
# Requires: jq, awk.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATES="$ROOT/docs/benchmarks/gates.toml"
FIXTURES="$ROOT/docs/benchmarks/fixtures"

usage() { sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }

for tool in jq awk; do
    command -v "$tool" >/dev/null || { echo "bench-compare: missing $tool" >&2; exit 2; }
done

# Flatten the gates.toml subset to "metric\t<m>", "noise\t<n>" and
# "gate\t<id>\t<kind>\t<limit>\t<noise>" (a gate's own `noise` overrides the global one; - = unset).
parse_gates() {
    awk '
        function val(s) { sub(/^[^=]*=[[:space:]]*/, "", s); sub(/[[:space:]]*#.*$/, "", s); gsub(/"/, "", s); return s }
        function flush() { if (id != "") print "gate\t" id "\t" kind "\t" (limit == "" ? "-" : limit) "\t" (gnoise == "" ? "-" : gnoise); id = kind = limit = gnoise = "" }
        /^[[:space:]]*(#|$)/ { next }
        /^\[\[gate\]\]/ { flush(); in_gate = 1; next }
        /^[[:space:]]*id[[:space:]]*=/ && in_gate { id = val($0); next }
        /^[[:space:]]*kind[[:space:]]*=/ && in_gate { kind = val($0); next }
        /^[[:space:]]*limit[[:space:]]*=/ && in_gate { limit = val($0); next }
        /^[[:space:]]*noise[[:space:]]*=/ && in_gate { gnoise = val($0); next }
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
    local gates="$1" base="$2" new="$3" spec metric
    for f in "$gates" "$base" "$new"; do
        [[ -f "$f" ]] || { echo "bench-compare: no such file: $f" >&2; exit 2; }
    done
    spec="$(parse_gates "$gates")" || exit 2
    metric="$(awk -F'\t' '$1 == "metric" { print $2 }' <<<"$spec")"
    metric="${metric:-median}"

    echo "bench-compare: $(jq -r '.baseline + "@" + (.sha // "?")[0:12]' "$base") -> $(jq -r '.baseline + "@" + (.sha // "?")[0:12]' "$new") ($metric)"
    awk -F'\t' '
        FILENAME == ARGV[1] {
            if ($1 == "noise") noise = $2 + 0
            if ($1 == "gate") { n++; ids[n] = $2; kinds[n] = $3; limits[n] = $4; noises[n] = $5; listed[$2] = 1 }
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
                id = ids[i]; kind = kinds[i]; limit = limits[i]
                if (!num(base[id]) || !num(cur[id])) {
                    row(id, kind, limit, "-", "FAIL (missing in " (!num(base[id]) ? "base" : "new") ")")
                    continue
                }
                r = cur[id] / base[id]; rs = sprintf("%.3f", r)
                if (kind == "report") { row(id, kind, limit, rs, "REPORT"); continue }
                if (kind != "budget" && kind != "improve" || limit == "-") {
                    row(id, kind, limit, rs, "FAIL (bad gate)"); continue
                }
                band = (noises[i] != "" && noises[i] != "-") ? noises[i] + 0 : noise
                ok = kind == "budget" ? r <= limit + 0 : r < limit + 0
                if (ok) row(id, kind, limit, rs, "PASS")
                else if (band > 0 && r <= limit + band) row(id, kind, limit, rs, "NOISE (within " band ")")
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
    # name, new fixture, expected exit code, id expected to FAIL (- for none)
    while read -r name file want fail_id; do
        set +e
        out="$(compare "$GATES" "$FIXTURES/base.json" "$FIXTURES/$file" 2>&1)"
        got=$?
        set -e
        fail_rows="$(awk '$5 == "FAIL" { print $1 }' <<<"$out" | paste -sd, -)"
        if [[ "$got" == "$want" && "${fail_rows:--}" == "$fail_id" ]]; then
            echo "self-test: ok   $name (exit $got)"
        else
            echo "self-test: FAIL $name (exit $got, want $want; failed ${fail_rows:--}, want $fail_id)"
            printf '%s\n' "$out" | sed 's/^/    /'
            failed=1
        fi
    done <<'EOF'
identity base.json 1 gate/spawn_join_10k,gate/sleep_cpu_1k_x_1ms,gate/axum_request_oneshot
pass pass.json 0 -
fail-budget fail-budget.json 1 gate/async_bind_64
fail-improve fail-improve.json 1 gate/axum_request_oneshot
fail-missing fail-missing.json 1 gate/sleep_cpu_1k_x_1ms
EOF
    ((failed == 0)) || { echo "bench-compare: self-test FAILED"; exit 1; }
    echo "bench-compare: self-test passed"
}

gates_arg=""
args=()
while (($#)); do
    case "$1" in
        --self-test) self_test; exit 0 ;;
        --gates) gates_arg="${2:?--gates needs a file}"; shift 2 ;;
        -h | --help) usage ;;
        *) args+=("$1"); shift ;;
    esac
done
((${#args[@]} == 2)) || usage
compare "${gates_arg:-$GATES}" "${args[0]}" "${args[1]}"
