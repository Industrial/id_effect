#!/usr/bin/env bash
# Validate the platform scorecard and compute its indices (docs/scorecard/METHOD.md §5 and §5.1).
#
# Usage: bash scripts/scorecard.sh [--file FILE] --validate
#        bash scripts/scorecard.sh [--file FILE] --version V | --targets V
#        bash scripts/scorecard.sh [--file FILE] --json V [--write]
#        bash scripts/scorecard.sh --validate --self-test
# FILE defaults to docs/scorecard/scorecard.json. --version V scores id_effect with the scores
# measured at V (planned scores, §5.1, where V has none); --targets V always uses planned scores.
# Both print U, E and RT with ranks, then ≥80, <40, leader and median gap, in the §5.2 layout.
# --json V prints the --version result as JSON; --write stores it in docs/scorecard/results/vV.json.
# --self-test checks the real file against METHOD §5.2 and runs docs/scorecard/fixtures.
# Exits 1 on any FAIL (invalid scorecard, self-test mismatch), 2 on misuse.
# Requires: bash ≥ 4, jq ≥ 1.6 (plus awk, which formats numbers with printf "%.1f", and diff).
set -euo pipefail
export LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/docs/scorecard"
FIXTURES="$DIR/fixtures"

usage() { sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }

((BASH_VERSINFO[0] >= 4)) || { echo "scorecard: bash >= 4 required" >&2; exit 2; }
for tool in jq awk diff; do
    command -v "$tool" >/dev/null || { echo "scorecard: missing $tool" >&2; exit 2; }
done

# One line per rule the scorecard breaks (none = valid). The shape rules mirror
# scorecard.schema.json; the cross references are the ones a schema cannot express.
# shellcheck disable=SC2016 # jq program, not shell expansions
VALIDATE_JQ='
def semver: type == "string" and test("^[0-9]+\\.[0-9]+\\.[0-9]+$");
def score: type == "number" and . == floor and . >= 1 and . <= 100;
def stances: "build", "promote", "integrate", "maintain", "non-goal", "partial-non-goal";
def shape:
  (("frameworks", "groups", "rows", "competitor_order") as $k
    | select((.[$k] | type) != "array") | "\($k): must be an array"),
  (select((.tracks | type) != "object") | "tracks: must be an object");
def rules:
  . as $d
  | [$d.groups[].id] as $gids
  | ($d.tracks | keys) as $tids
  | [$d.frameworks[].id] as $fids
  | (select($fids[0] != "id_effect") | "frameworks: the first framework must be id_effect"),
    (select(($d.competitor_order | length) != 10)
      | "competitor_order: \($d.competitor_order | length) entries, want 10"),
    (select($fids[1:] != $d.competitor_order)
      | "competitor_order: must list the frameworks after id_effect, in order"),
    ($gids | group_by(.)[] | select(length > 1) | "groups: duplicate id \(.[0] | tojson)"),
    ($d.groups[] | select((.w_enterprise | type) != "number" or (.w_runtime | type) != "number")
      | "group \(.id | tojson): w_enterprise and w_runtime must be numbers"),
    (("w_enterprise", "w_runtime") as $w
      | select(([$d.groups[][$w] | numbers] | add // 0) <= 0) | "groups: \($w) must sum to > 0"),
    ($gids[] as $g | select(any($d.rows[]; .group == $g) | not) | "group \($g | tojson): no rows"),
    ($d.rows | map(.id) | group_by(.)[] | select(length > 1) | "rows: duplicate id \(.[0] | tojson)"),
    ($d.rows[] | "row \(.id | tojson)" as $at
      | (select((.id | type) != "string") | "\($at): id must be a string"),
        (select((.competitors | type) != "array" or (.competitors | length) != 10)
          | "\($at): want exactly 10 competitor scores"),
        (.competitors | arrays | to_entries[] | select(.value | score | not)
          | "\($at): competitors[\(.key)] = \(.value | tojson) is not an integer in 1..100"),
        (select((.id_effect | type) != "object" or (.id_effect | length) == 0)
          | "\($at): id_effect must be a non-empty object"),
        (select((.targets | type) != "object") | "\($at): targets must be an object"),
        (. as $r | ("id_effect", "targets") as $k | $r[$k] | objects | to_entries[]
          | (select(.key | semver | not) | "\($at): \($k) key \(.key | tojson) is not x.y.z"),
            (select(.value | score | not)
              | "\($at): \($k)[\(.key)] = \(.value | tojson) is not an integer in 1..100")),
        (.group as $g | select(any($gids[]; . == $g) | not) | "\($at): unknown group \($g | tojson)"),
        (.track as $t | select(any($tids[]; . == $t) | not) | "\($at): unknown track \($t | tojson)"),
        (.stance as $s | select(any(stances; . == $s) | not)
          | "\($at): unknown stance \($s | tojson)"));
(select(type != "object") | "top level: must be an object"),
(objects | [shape] as $e | $e[], (select($e == []) | rules))
'

# METHOD §5 on a valid scorecard, as raw (unrounded) TSV records:
#   meta <basis> <method> <snapshot>
#   fw <id> <name> <U> <E> <RT> <rank U> <rank E> <rank RT> <display position, 0 = id_effect>
#   grp <framework> <group> <group score>
#   kpi <≥80> <<40> <leader> <median gap>
# shellcheck disable=SC2016 # jq program, not shell expansions
COMPUTE_JQ='
def vkey: split(".") | map(tonumber);
def mean: add / length;
# §5.1: max(latest measured score at or before $v, max target score at or before $v). A version
# older than every measured one falls back to the latest measured score.
def planned($v):
  ($v | vkey) as $k
  | (.id_effect | to_entries | sort_by(.key | vkey)) as $m
  | (([$m[] | select((.key | vkey) <= $k)] | last) // ($m | last) | .value) as $base
  | [$base, (.targets | to_entries[] | select((.key | vkey) <= $k) | .value)] | max;
def score($v; $mode): (select($mode == "version") | .id_effect[$v]) // planned($v);
. as $d
| (("measured" | select($mode == "version" and all($d.rows[]; .id_effect[$v] != null)))
   // "target") as $basis
| [$d.rows[] | {group, s: ([score($v; $mode)] + .competitors)}] as $m
| [$d.groups[].id] as $gids
| ([$d.groups[].w_enterprise] | add) as $we
| ([$d.groups[].w_runtime] | add) as $wr
| [range(0; $d.frameworks | length) as $i
    | [$gids[] as $g | [$m[] | select(.group == $g) | .s[$i]] | mean] as $gm
    | {id: $d.frameworks[$i].id, name: $d.frameworks[$i].name,
       u: ([$m[].s[$i]] | mean),
       e: ([range(0; $gids | length) as $j | $d.groups[$j].w_enterprise * $gm[$j]] | add / $we),
       rt: ([range(0; $gids | length) as $j | $d.groups[$j].w_runtime * $gm[$j]] | add / $wr),
       groups: [range(0; $gids | length) as $j | [$gids[$j], $gm[$j]]]}] as $ix
| ($ix[1:] | sort_by(-.u) | to_entries | map({key: .value.id, value: (.key + 1)}) | from_entries)
  as $pos
| [$m[].s[0]] as $ids
| ([$m[] | (.s[1:] | max) - .s[0]] | sort) as $gaps
| ["meta", $basis, $d.method_version, $d.competitor_snapshot.date],
  ($ix[] as $f
    | ["fw", $f.id, $f.name, $f.u, $f.e, $f.rt]
      + [("u", "e", "rt") as $k | 1 + ([$ix[] | select(.[$k] > $f[$k])] | length)]
      + [$pos[$f.id] // 0]),
  ($ix[] as $f | $f.groups[] | ["grp", $f.id, .[0], .[1]]),
  ["kpi", ([$ids[] | select(. >= 80)] | length), ([$ids[] | select(. < 40)] | length),
   ([$m[] | select(.s[0] >= (.s[1:] | max))] | length), $gaps[$gaps | length / 2 | floor]]
| @tsv
'

# Exit 2 unless FILE exists; return 1 (FAIL lines on stderr) if it breaks any rule.
check() {
    local file="$1" errs line
    [[ -f "$file" ]] || { echo "scorecard: no such file: $file" >&2; exit 2; }
    if ! errs="$(jq -r "$VALIDATE_JQ" "$file" 2>&1)"; then
        echo "scorecard: FAIL ${file#"$ROOT"/}: ${errs:-unreadable}" >&2
        return 1
    fi
    [[ -z "$errs" ]] && return 0
    while IFS= read -r line; do echo "scorecard: FAIL ${file#"$ROOT"/}: $line" >&2; done <<<"$errs"
    return 1
}

# compute FILE V MODE (version|targets): the COMPUTE_JQ records, indices formatted to one decimal.
# awk formats the doubles jq printed (C printf rounding, like the §5.2 reference); jq never rounds.
compute() {
    jq -r --arg v "$2" --arg mode "$3" "$COMPUTE_JQ" "$1" | awk -F'\t' -v OFS='\t' '
        $1 == "fw" { for (i = 4; i <= 6; i++) $i = sprintf("%.1f", $i) }
        $1 == "grp" { $4 = sprintf("%.1f", $4) }
        { print }'
}

# render V PART < compute output. PART: all, id (the id_effect row of the §5.2 version table) or
# competitors (the rows of the §5.2 competitor table, by U descending).
render() {
    awk -F'\t' -v v="$1" -v part="$2" '
        $1 == "meta" { basis = $2; method = $3; snapshot = $4 }
        $1 == "fw" && $10 == 0 { id = sprintf("| %s %s | %s | %s (#%s) | %s (#%s) |", v, basis, $4, $5, $8, $6, $9); urank = $7 }
        $1 == "fw" && $10 > 0 { comp[$10] = sprintf("| %s | %s | %s | %s |", $3, $4, $5, $6); n++ }
        $1 == "kpi" { kpi = sprintf(" %s | %s | %s | %s |", $2, $3, $4, $5) }
        END {
            if (part == "id") { print id kpi; exit }
            if (part != "competitors") {
                printf "scorecard: %s %s (method %s, competitor snapshot %s)\n\n", v, basis, method, snapshot
                print "| Version | U | E (rank) | RT (rank) | ≥80 | <40 | leader | median gap |"
                print "|---|---|---|---|---|---|---|---|"
                print id kpi
                printf "\nid_effect U rank: #%s of %d\n\n", urank, n + 1
                print "| Framework | U | E | RT |"
                print "|---|---|---|---|"
            }
            for (i = 1; i <= n; i++) print comp[i]
        }'
}

# render_json V < compute output. Tab-indented and fully expanded, which biome (treefmt) keeps as is.
render_json() {
    # shellcheck disable=SC2016 # jq program, not shell expansions
    jq -R -n --tab --arg v "$1" '
        def num: tonumber + 0;
        [inputs | split("\t")] as $l
        | ($l | map(select(.[0] == "meta")) | first) as $meta
        | ($l | map(select(.[0] == "kpi")) | first | .[1:] | map(num)) as $kpi
        | [$l[] | select(.[0] == "fw") | . as $f
            | {id: .[1], name: .[2], u: (.[3] | num), e: (.[4] | num), rt: (.[5] | num),
               rank: {u: (.[6] | num), e: (.[7] | num), rt: (.[8] | num)},
               groups: ([$l[] | select(.[0] == "grp" and .[1] == $f[1])
                 | {key: .[2], value: (.[3] | num)}] | from_entries)}] as $fw
        | {version: $v, basis: $meta[1], method_version: $meta[2], competitor_snapshot: $meta[3],
           id_effect: (($fw[0] | {u, e, rt, rank})
             + {ge80: $kpi[0], lt40: $kpi[1], leader: $kpi[2], median_gap: $kpi[3]}),
           frameworks: $fw}'
}

# id_rows FILE MODE:V...: the id_effect row (render ... id) for each spec.
id_rows() {
    local file="$1" spec
    shift
    for spec in "$@"; do
        compute "$file" "${spec#*:}" "${spec%%:*}" | render "${spec#*:}" id
    done
}

self_test() {
    local file="$DIR/scorecard.json" failed=0 want got out rc name msg
    expect() { # name, want, got
        if [[ "$2" == "$3" ]]; then
            echo "self-test: ok   $1"
        else
            echo "self-test: FAIL $1"
            diff -u --label want --label got <(printf '%s\n' "$2") <(printf '%s\n' "$3") | sed 's/^/    /' || true
            failed=1
        fi
    }

    if out="$(check "$file" 2>&1)"; then
        echo "self-test: ok   validate ${file#"$ROOT"/}"
    else
        echo "self-test: FAIL validate ${file#"$ROOT"/}"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=1
    fi

    # METHOD.md §5.2, copied verbatim.
    read -r -d '' want <<'EOF' || true
| 0.6.0 measured | 45.9 | 44.0 (#11) | 55.4 (#8) | 12 | 34 | 2 | 47 |
| 0.7.0 measured | 49.1 | 46.8 (#11) | 59.1 (#7) | 16 | 31 | 3 | 40 |
| 0.8.0 target | 55.8 | 54.3 (#11) | 64.3 (#7) | 25 | 24 | 4 | 28 |
| 0.9.0 target | 63.3 | 62.8 (#9) | 68.2 (#6) | 35 | 18 | 4 | 20 |
| 0.10.0 target | 69.6 | 69.4 (#5) | 74.1 (#4) | 41 | 11 | 6 | 15 |
| 0.11.0 target | 73.1 | 72.3 (#4) | 78.6 (#2) | 50 | 8 | 8 | 12 |
| 0.12.0 target | 76.7 | 76.5 (#4) | 80.9 (#1) | 51 | 3 | 8 | 12 |
| 1.0.0 target | 80.4 | 80.4 (#3) | 83.5 (#1) | 59 | 1 | 10 | 11 |
EOF
    got="$(id_rows "$file" version:0.6.0 version:0.7.0)"
    got+=$'\n'"$(id_rows "$file" targets:0.8.0 targets:0.9.0 targets:0.10.0 targets:0.11.0)"
    got+=$'\n'"$(id_rows "$file" targets:0.12.0 targets:1.0.0)"
    expect "METHOD §5.2 version table" "$want" "$got"

    read -r -d '' want <<'EOF' || true
| .NET | 83.9 | 85.4 | 80.3 |
| Spring | 78.8 | 81.1 | 74.3 |
| Phoenix | 77.6 | 77.6 | 75.4 |
| Laravel | 69.1 | 71.3 | 57.5 |
| Effect-TS | 67.9 | 67.1 | 69.8 |
| Rails | 65.9 | 67.2 | 54.3 |
| ZIO | 65.7 | 63.8 | 68.7 |
| Django | 62.9 | 64.3 | 50.8 |
| OTP | 58.9 | 57.8 | 65.0 |
| Next.js | 56.9 | 58.7 | 49.2 |
EOF
    expect "METHOD §5.2 competitor table" "$want" "$(compute "$file" 0.7.0 version | render 0.7.0 competitors)"

    # name, expected message: each invalid fixture must make --validate exit 1 for that reason.
    while read -r name msg; do
        set +e
        out="$(check "$FIXTURES/$name.json" 2>&1)"
        rc=$?
        set -e
        if [[ "$rc" == 1 && "$out" == *"$msg"* ]]; then
            echo "self-test: ok   fixture $name (exit 1)"
        else
            echo "self-test: FAIL fixture $name (exit $rc, want 1 with \"$msg\")"
            printf '%s\n' "$out" | sed 's/^/    /'
            failed=1
        fi
    done <<'EOF'
dup-id duplicate id "core.a"
bad-score competitors[3] = 101 is not an integer in 1..100
unknown-group unknown group "nope"
EOF

    # Ties take the better rank (§5 step 5); id_effect also ties the best competitor on row core.b,
    # which counts as leader. 0.2.0 has no measured scores or targets, so it falls back to 0.1.0.
    file="$FIXTURES/valid/tie.json"
    if out="$(check "$file" 2>&1)"; then
        echo "self-test: ok   validate fixture valid/tie"
    else
        echo "self-test: FAIL validate fixture valid/tie"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=1
    fi
    read -r -d '' want <<'EOF' || true
| 0.1.0 measured | 50.0 | 50.0 (#2) | 50.0 (#2) | 0 | 0 | 1 | 40 |
| 0.2.0 target | 50.0 | 50.0 (#2) | 50.0 (#2) | 0 | 0 | 1 | 40 |
| 0.3.0 target | 65.0 | 65.0 (#1) | 65.0 (#1) | 1 | 0 | 1 | 10 |
EOF
    got="$(id_rows "$file" version:0.1.0 version:0.2.0 targets:0.3.0)"
    expect "fixture valid/tie ranks and fallback" "$want" "$got"

    ((failed == 0)) || { echo "scorecard: self-test FAILED"; exit 1; }
    echo "scorecard: self-test passed"
}

file="$DIR/scorecard.json"
write=0
actions=()
while (($#)); do
    case "$1" in
        --validate | --self-test) actions+=("${1#--}"); shift ;;
        --version | --targets | --json)
            [[ "${2:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "scorecard: $1 needs a version x.y.z" >&2; exit 2; }
            actions+=("${1#--}:$2")
            shift 2
            ;;
        --file)
            [[ -n "${2:-}" ]] || { echo "scorecard: --file needs a path" >&2; exit 2; }
            file="$2"
            shift 2
            ;;
        --write) write=1; shift ;;
        -h | --help) usage ;;
        *) echo "scorecard: unknown argument: $1" >&2; usage ;;
    esac
done
((${#actions[@]})) || usage
if ((write)) && [[ " ${actions[*]} " != *" json:"* ]]; then
    echo "scorecard: --write needs --json" >&2
    exit 2
fi

for action in "${actions[@]}"; do
    case "$action" in
        validate)
            check "$file"
            echo "scorecard: ${file#"$ROOT"/} valid ($(jq '.rows | length' "$file") rows)"
            ;;
        self-test) self_test ;;
        version:* | targets:*)
            check "$file"
            compute "$file" "${action#*:}" "${action%%:*}" | render "${action#*:}" all
            ;;
        json:*)
            check "$file"
            out="$(compute "$file" "${action#*:}" version | render_json "${action#*:}")"
            if ((write)); then
                mkdir -p "$DIR/results"
                printf '%s\n' "$out" >"$DIR/results/v${action#*:}.json"
                echo "scorecard: wrote docs/scorecard/results/v${action#*:}.json"
            else
                printf '%s\n' "$out"
            fi
            ;;
    esac
done
