#!/usr/bin/env bash
# Count inference annotations (turbofish, explicit R/index types) in spike corpus code.
#
# Usage:
#   spikes/scripts/count-annotations.sh [--json] [--corpus FILE|none] [--ids "H01 H02 ..."] FILE...
#   spikes/scripts/count-annotations.sh --self-test
#
# Regions are delimited by `// corpus:begin <ID>` / `// corpus:end <ID>`; a shape the design cannot
# express is declared with `// corpus:unsupported <ID> <reason>`. Every id listed in the corpus
# (default spikes/corpus/handlers.md, `### H01 — ...` headings) that no file declares is reported as
# `missing`. A file without any markers is counted as one region named after the file.
#
# Output (TSV, or JSON with --json): one row per region with the columns
#   turbofish   `::<` occurrences
#   let_ty      `let x: T` / `let (a, b): T` ascriptions
#   closure_ty  closures with an annotated parameter (`|r: &mut R|`)
#   marker      lines tagged `// ann: <why>` (annotations the patterns cannot see)
#   total       turbofish + let_ty + closure_ty + marker
#   index       Design A index witnesses named in code (`Here`, `There<..>`); informational
#   fwd         entries in `forward(...)` lists (Design B's falsifier); informational
# then a `summary` row: annotated_pct = regions with total > 0 / ported regions.
# fn signatures are never counted. Full rules: spikes/corpus/README.md.
#
# Exit status: 0 ok, 1 malformed markers (or self-test failure), 2 usage error.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPIKES_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
AWK_PROG="${SCRIPT_DIR}/lib/count-annotations.awk"

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed -E 's/^# ?//'
}

corpus_ids_from() {
    grep -oE '^### [A-Z][0-9]+' "$1" | awk '{ print $2 }' | tr '\n' ' '
}

self_test() {
    local fixtures="${SCRIPT_DIR}/fixtures" tmp got
    tmp="$(mktemp -d)"
    trap 'rm -rf "${tmp}"' RETURN

    got="$("${BASH_SOURCE[0]}" --ids "H01 H02 H03 H04 H05" "${fixtures}/annotations.rs.fixture")"
    if ! diff -u "${fixtures}/annotations.expected.tsv" <(printf '%s\n' "${got}"); then
        echo "self-test FAILED: marker mode" >&2
        return 1
    fi

    "${BASH_SOURCE[0]}" --json --ids "H01 H02 H03 H04 H05" "${fixtures}/annotations.rs.fixture" >"${tmp}/out.json"
    if ! grep -q '"summary": {"handlers": 6, "ported": 4, "unsupported": 1, "missing": 1, "annotated": 3, "annotated_pct": 75.0, "total": 6, "index": 3, "fwd": 5}' "${tmp}/out.json"; then
        echo "self-test FAILED: json summary" >&2
        cat "${tmp}/out.json" >&2
        return 1
    fi

    grep -v 'corpus:' "${fixtures}/annotations.rs.fixture" >"${tmp}/plain.rs"
    got="$("${BASH_SOURCE[0]}" "${tmp}/plain.rs" | tail -n 1)"
    if [[ "${got}" != *"handlers=1 ported=1 unsupported=0 missing=0 annotated=1 annotated_pct=100.0 total=6 index=3 fwd=5" ]]; then
        echo "self-test FAILED: whole-file mode: ${got}" >&2
        return 1
    fi

    printf '// corpus:begin H01\nfn a() {}\n' >"${tmp}/unclosed.rs"
    if "${BASH_SOURCE[0]}" --corpus none "${tmp}/unclosed.rs" >/dev/null 2>&1; then
        echo "self-test FAILED: an unclosed region must exit 1" >&2
        return 1
    fi

    if [[ -z "$(corpus_ids_from "${SPIKES_DIR}/corpus/handlers.md")" ]]; then
        echo "self-test FAILED: no shape ids parsed from corpus/handlers.md" >&2
        return 1
    fi

    echo "count-annotations self-test: ok"
}

json=0
corpus="${SPIKES_DIR}/corpus/handlers.md"
ids=""
files=()
while (($# > 0)); do
    case "$1" in
        --self-test)
            self_test
            exit
            ;;
        --json)
            json=1
            ;;
        --corpus)
            corpus="${2:?--corpus needs a file or 'none'}"
            shift
            ;;
        --ids)
            ids="${2:?--ids needs a space-separated list}"
            corpus=none
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            echo "count-annotations: unknown option $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            files+=("$1")
            ;;
    esac
    shift
done

if ((${#files[@]} == 0)); then
    usage >&2
    exit 2
fi
for f in "${files[@]}"; do
    [[ -f "${f}" ]] || {
        echo "count-annotations: no such file: ${f}" >&2
        exit 2
    }
done
if [[ "${corpus}" != none ]]; then
    [[ -f "${corpus}" ]] || {
        echo "count-annotations: no such corpus: ${corpus}" >&2
        exit 2
    }
    ids="$(corpus_ids_from "${corpus}")"
fi

exec awk -v corpus_ids="${ids}" -v json="${json}" -f "${AWK_PROG}" "${files[@]}"
