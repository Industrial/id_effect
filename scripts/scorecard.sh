#!/usr/bin/env bash
# Validate the platform scorecard, compute its indices (docs/scorecard/METHOD.md §5 and §5.1) and
# publish them (METHOD §6, PLAN-OF-PLANS §7.3 and §7.4).
#
# Usage: bash scripts/scorecard.sh [--file FILE] --validate
#        bash scripts/scorecard.sh [--file FILE] --version V | --targets V
#        bash scripts/scorecard.sh [--file FILE] --json V [--write]
#        bash scripts/scorecard.sh [--file FILE] --diff A B | --compare-targets V
#        bash scripts/scorecard.sh [--file FILE] --report V [--prev P] | --html
#        bash scripts/scorecard.sh [--file FILE] --method 1|2 --version V | --targets V | --json V | --diff A B
#        bash scripts/scorecard.sh [--file FILE] --method-compare
#        bash scripts/scorecard.sh --validate --self-test
# FILE defaults to docs/scorecard/scorecard.json. --version V scores id_effect with the scores
# measured at V (planned scores, §5.1, where V has none); --targets V always uses planned scores.
# Both print U, E and RT with ranks, then ≥80, <40, leader and median gap, in the §5.2 layout.
# Scores are read by the file's own method (METHOD §7.4: the major of method_version, 2 since the
# 2.0 re-base). --method 1 on a method 2 file reads the method 1.1 history (rows[].v1); --method 2
# on a method 1 file replaces every measured score by its checklist score and needs a measured V.
# --method-compare writes docs/scorecard/REPORT-method-v2.md: v1 beside v2 at each measured version.
# --json V prints the --version result as JSON; --write stores it in docs/scorecard/results/vV.json.
# --diff A B lists the rows whose measured id_effect score changed from A to B; it FAILs on a drop
# of 5 or more unless the row has an adr note for B. --compare-targets V sets the indices measured
# at V beside the planned ones; it FAILs if measured E < planned E − 1.0.
# --report V writes docs/scorecard/REPORT-V.md, comparing V with P (default: the measured version
# before V). --html writes docs/scorecard/index.html: docs/scorecard/template.html with the data.
# --self-test checks the real file against METHOD §5.2 and runs docs/scorecard/fixtures.
# Exits 1 on any FAIL (invalid scorecard, self-test mismatch, missed gate), 2 on misuse.
# Requires: bash ≥ 4, jq ≥ 1.6 (plus awk, which formats numbers with printf "%.1f", diff, cmp, mktemp).
set -euo pipefail
export LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/docs/scorecard"
FIXTURES="$DIR/fixtures"

usage() { sed -n '2,/^# Requires/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }

((BASH_VERSINFO[0] >= 4)) || { echo "scorecard: bash >= 4 required" >&2; exit 2; }
for tool in jq awk diff cmp mktemp; do
    command -v "$tool" >/dev/null || { echo "scorecard: missing $tool" >&2; exit 2; }
done

# Generated files are built here first and copied into place whole.
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

# One line per rule the scorecard breaks (none = valid). The shape rules mirror
# scorecard.schema.json; the cross references are the ones a schema cannot express.
# shellcheck disable=SC2016 # jq program, not shell expansions
VALIDATE_JQ='
def semver: type == "string" and test("^[0-9]+\\.[0-9]+\\.[0-9]+$");
def score: type == "number" and . == floor and . >= 1 and . <= 100;
def stances: "build", "promote", "integrate", "maintain", "non-goal", "partial-non-goal";
def level: . as $l | any("absent", "labs", "published", "tested", "documented", "production"; . == $l);
# METHOD §7 on a row with checks: 4–8 checks with unique "<row id>#" ids; a v2 level and met list
# for every framework (id_effect: one per version it has a score for); only known check ids.
def known($ids): . as $c | any($ids[]; . == $c);
def met_list($at; $where; $ids):
  if type != "array" then "\($at): \($where) must be an array of check ids" else
    (.[] | select(known($ids) | not) | "\($at): \($where) has unknown check id \(tojson)"),
    (group_by(.)[] | select(length > 1) | "\($at): \($where) lists \(.[0] | tojson) twice")
  end;
def v2rules($at; $fids):
  . as $r
  | ([.checks | arrays | .[] | objects | .id]) as $ids
  | (($r.id_effect | objects | keys) // []) as $vs
  | (select((.checks | type) != "array" or (.checks | length) < 4 or (.checks | length) > 8)
      | "\($at): checks must be an array of 4 to 8 entries"),
    (.checks | arrays | .[] | select((.id | type) != "string" or (.text | type) != "string" or .text == "")
      | "\($at): every check needs a string id and a non-empty text"),
    ($ids | group_by(.)[] | select(length > 1) | "\($at): duplicate check id \(.[0] | tojson)"),
    ($ids[] | strings | select(startswith("\($r.id)#") | not)
      | "\($at): check id \(tojson) must start with \("\($r.id)#" | tojson)"),
    (select((.v2 | type) != "object" or (.v2.level | type) != "object" or (.v2.met | type) != "object")
      | "\($at): checks need a v2 object with level and met objects"),
    (.v2 | objects
      | (("level", "met") as $k | .[$k] | objects
          | (($fids - keys)[] | "\($at): v2.\($k) has no entry for \(tojson)"),
            ((keys - $fids)[] | "\($at): v2.\($k) has unknown framework \(tojson)")),
        (("level", "met") as $k | .[$k] | objects | .id_effect
          | select(type != "object" or keys != $vs)
          | "\($at): v2.\($k).id_effect must have one entry per id_effect version \($vs | tojson)"),
        (.level | objects | to_entries[]
          | if .key == "id_effect" then
              .value | objects | to_entries[] | select(.value | level | not)
              | "\($at): v2.level.id_effect[\(.key)] = \(.value | tojson) is not a level"
            else
              select(.value | level | not) | "\($at): v2.level.\(.key) = \(.value | tojson) is not a level"
            end),
        (.met | objects | to_entries[]
          | if .key == "id_effect" then
              .value | objects | to_entries[] | .key as $v
              | .value | met_list($at; "v2.met.id_effect[\($v)]"; $ids)
            else
              .key as $f | .value | met_list($at; "v2.met.\($f)"; $ids)
            end),
        (("evidence", "notes") as $k | .[$k]
          | (select(. != null and type != "object") | "\($at): v2.\($k) must be an object"),
            (objects | to_entries[]
              | (select($k == "evidence" and (.key | known($ids) | not))
                  | "\($at): v2.evidence key \(.key | tojson) is not a check id"),
                (select($k == "notes" and (.key | known($fids) | not))
                  | "\($at): v2.notes key \(.key | tojson) is not a framework"),
                (select((.value | type) != "string" or .value == "")
                  | "\($at): v2.\($k)[\(.key | tojson)] must be a non-empty string"))));
# METHOD §7.4, method 2.x: every stored score is its checklist score, and rows[].v1 keeps the
# method 1.1 scores and targets (--method 1).
def stored2($at; $order):
  . as $r | (.checks | length) as $n | .v2 as $w
  | (try (
      ([$order[] as $f | v2score($w.level[$f]; $w.met[$f]; $n)] as $want
        | range(0; 10) | select($r.competitors[.] != $want[.])
        | "\($at): competitors[\(.)] = \($r.competitors[.]) is not its checklist score \($want[.])"),
      ($r.id_effect | to_entries[]
        | v2score($w.level.id_effect[.key]; $w.met.id_effect[.key]; $n) as $want
        | select(.value != $want)
        | "\($at): id_effect[\(.key)] = \(.value) is not its checklist score \($want)"))
    catch "\($at): cannot compute its checklist scores"),
    (select((.v1 | type) != "object") | "\($at): method 2 rows need a v1 object (method 1.1)"),
    (.v1 | objects
      | (select((.competitors | type) != "array" or (.competitors | length) != 10
            or any(.competitors[]; score | not))
          | "\($at): v1.competitors must be 10 integers in 1..100"),
        (select((.id_effect | type) != "object" or ((.id_effect | keys) - ($r.id_effect | keys)) != []
            or any(.id_effect[]; score | not))
          | "\($at): v1.id_effect must score versions of id_effect (history may stop earlier), each an integer in 1..100"),
        (select((.targets | type) != "object" or any(.targets[]; score | not))
          | "\($at): v1.targets must map versions to integers in 1..100"));
# METHOD §7.4 (2.1): on a process row, a framework is production when it meets a check, else absent.
def process_levels($at):
  .v2 as $w
  | (($w.level.id_effect | objects | to_entries[]
        | {f: "id_effect[\(.key)]", l: .value, m: ($w.met.id_effect[.key] // [])}),
      ($w.level | objects | to_entries[] | select(.key != "id_effect")
        | {f: .key, l: .value, m: ($w.met[.key] // [])}))
  | ["absent", "production"][[.m | length, 1] | min] as $want
  | select(.l != $want)
  | "\($at): process row: v2.level.\(.f) = \(.l | tojson), want \($want | tojson)";
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
          | "\($at): unknown stance \($s | tojson)"),
        (select(has("adr") and (.adr | type) != "object") | "\($at): adr must be an object"),
        (.adr | objects | to_entries[]
          | (select(.key | semver | not) | "\($at): adr key \(.key | tojson) is not x.y.z"),
            (select((.value | type) != "string" or .value == "")
              | "\($at): adr[\(.key)] must be a non-empty string")),
        (select(has("v2") and (has("checks") | not)) | "\($at): v2 needs checks"),
        (select(has("checks")) | v2rules($at; $fids)),
        (select(($d.method_version | tostring | startswith("2")) and has("checks"))
          | stored2($at; $d.competitor_order)),
        (select(has("process") and (.process | type) != "boolean") | "\($at): process must be a boolean"),
        (select(.process == true and has("checks")) | process_levels($at)));
(select(type != "object") | "top level: must be an object"),
(objects | [shape] as $e | $e[], (select($e == []) | rules))
'

# Definitions shared by the jq programs below.
# shellcheck disable=SC2016 # jq program, not shell expansions
SCORE_JQ='
def vkey: split(".") | map(tonumber);
# §5.1: max(latest measured score at or before $v, max target score at or before $v). A version
# older than every measured one falls back to the latest measured score.
def planned($v):
  ($v | vkey) as $k
  | (.id_effect | to_entries | sort_by(.key | vkey)) as $m
  | (([$m[] | select((.key | vkey) <= $k)] | last) // ($m | last) | .value) as $base
  | [$base, (.targets | to_entries[] | select((.key | vkey) <= $k) | .value)] | max;
def score($v; $mode): (select($mode == "version") | .id_effect[$v]) // planned($v);
# The versions every row has a measured score for, then the other target versions, ascending.
def measured:
  . as $d | [$d.rows[].id_effect | keys[]] | unique
  | map(select(. as $v | all($d.rows[]; .id_effect[$v] != null))) | sort_by(vkey);
def target_versions: ([.rows[].targets | keys[]] | unique) - measured | sort_by(vkey);
# METHOD §7: s = clamp(1, cap(level), round(100 × met / checks)), the caps of §3.2. round is half
# away from zero, done in integers: floor((200 × met + checks) / (2 × checks)).
def cap: {absent: 9, labs: 39, published: 49, tested: 69, documented: 89, production: 100}[.];
def v2score($level; $met; $n):
  [1, ([($level | cap), ((200 * ($met | length) + $n) / (2 * $n) | floor)] | min)] | max;
# The method (1 or 2) a scorecard stores its scores in: the major of method_version.
def primary: .method_version | tostring | split(".")[0] | tonumber? // 1;
# A method 2.x scorecard read by method 1.1: the scores and targets of each row from rows[].v1.
def v1doc:
.method_version = "1.1"
| .rows |= map(.competitors = .v1.competitors | .id_effect = .v1.id_effect | .targets = .v1.targets | del(.v1));
# A method 1.x scorecard with every measured id_effect score and every competitor score replaced by
# its v2 score. targets stay v1, so only measured versions are meaningful.
def v2doc:
.competitor_order as $order
| .method_version = "v2 checklists"
| .rows |= map((.checks | length) as $n | .v2 as $w
    | .id_effect |= with_entries(.value = v2score($w.level.id_effect[.key]; $w.met.id_effect[.key]; $n))
| .competitors = [$order[] as $f | v2score($w.level[$f]; $w.met[$f]; $n)]);
# Markdown table cell text.
def md: gsub("\\\\"; "\\\\") | gsub("\\|"; "\\|") | gsub("<"; "\\<") | gsub("\\*"; "\\*");
    '

# METHOD §5 on a valid scorecard, as raw (unrounded) TSV records:
#   meta <basis> <method> <snapshot>
#   fw <id> <name> <U> <E> <RT> <rank U> <rank E> <rank RT> <display position, 0 = id_effect>
#   grp <framework> <group> <group score>
#   kpi <≥80> <<40> <leader> <median gap>
# shellcheck disable=SC2016 # jq program, not shell expansions
COMPUTE_JQ="$SCORE_JQ"'
    def mean: add / length;
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
    if ! errs="$(jq -r "$SCORE_JQ$VALIDATE_JQ" "$file" 2>&1)"; then
        echo "scorecard: FAIL ${file#"$ROOT"/}: ${errs:-unreadable}" >&2
        return 1
    fi
    [[ -z "$errs" ]] && return 0
    while IFS= read -r line; do echo "scorecard: FAIL ${file#"$ROOT"/}: $line" >&2; done <<<"$errs"
    return 1
}

# compute_raw FILE V MODE (version|targets): the COMPUTE_JQ records, unrounded.
compute_raw() { jq -r --arg v "$2" --arg mode "$3" "$COMPUTE_JQ" "$1"; }

# compute FILE V MODE: the COMPUTE_JQ records, indices formatted to one decimal.
# awk formats the doubles jq printed (C printf rounding, like the §5.2 reference); jq never rounds.
compute() {
    compute_raw "$@" | awk -F'\t' -v OFS='\t' '
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

# versions FILE KIND (measured|target_versions): one version per line, ascending.
versions() { jq -r "$SCORE_JQ$2"'[]' "$1"; }

# primary_method FILE: the method FILE stores its scores in (1 or 2, METHOD §7.4).
primary_method() { jq -r "$SCORE_JQ"'primary' "$1"; }

# rebase FILE: a method 1 FILE scored by method 2 (v2doc), written to the scratch directory; prints
# its path. Exit 2 unless every row of FILE has checks.
rebase() {
    jq -e 'all(.rows[]; has("checks"))' "$1" >/dev/null || {
        echo "scorecard: method 2 needs checks on every row of ${1#"$ROOT"/}" >&2
        exit 2
    }
    jq "$SCORE_JQ"'v2doc' "$1" >"$SCRATCH/method2.json"
    printf '%s\n' "$SCRATCH/method2.json"
}

# historic FILE: a method 2 FILE read by method 1.1 (v1doc), written to the scratch directory;
# prints its path.
historic() {
    jq "$SCORE_JQ"'v1doc' "$1" >"$SCRATCH/method1.json"
    printf '%s\n' "$SCRATCH/method1.json"
}

# by_method FILE M: FILE scored by method M (1 or 2); prints a path (FILE itself for its own method).
by_method() {
    local primary
    primary="$(primary_method "$1")"
    if [[ "$2" == "$primary" ]]; then
        printf '%s\n' "$1"
    elif [[ "$2" == 2 ]]; then
        rebase "$1"
    else
        historic "$1"
    fi
}

# Exit 2 unless every row of FILE has a measured id_effect score at V.
require_measured() {
    # shellcheck disable=SC2016 # jq program, not shell expansions
    jq -e --arg v "$2" "$SCORE_JQ"'any(measured[]; . == $v)' "$1" >/dev/null || {
        echo "scorecard: $2 has no measured id_effect scores in ${1#"$ROOT"/}" >&2
        exit 2
    }
}

# The rows whose measured id_effect score changed from $a to $b, as markdown ($part: table, the
# rows that went down, the no-silent-regression gate line) or as FAIL lines ($part: fails). A drop
# of 5 or more needs an adr note for $b (PLAN-OF-PLANS §7.3).
# shellcheck disable=SC2016 # jq program, not shell expansions
CHANGES_JQ="$SCORE_JQ"'
def signed: if . > 0 then "+\(.)" else tostring end;
def adr_note: if .adr then " (ADR: \(.adr | md))" else "" end;
(.rows | length) as $n
| [.rows[] | select(.id_effect[$a] != .id_effect[$b])
    | {id, name, evidence, old: .id_effect[$a], new: .id_effect[$b],
       delta: (.id_effect[$b] - .id_effect[$a]), adr: (.adr[$b] // null)}]
| map(select(.delta <= -5)) as $drops
| if $part == "fails" then
    $drops[] | select(.adr == null)
    | "row \(.id | tojson): \(.old) at \($a), \(.new) at \($b) (\(.delta)) with no adr note for \($b)"
  elif $part == "table" then
    if length == 0 then "No row changed from \($a) to \($b) (\($n) rows)." else
      "| Row | Capability | \($a) | \($b) | Δ | Evidence |",
      "|---|---|---|---|---|---|",
      (.[] | "| `\(.id)` | \(.name | md) | \(.old) | \(.new) | \(.delta | signed) | \(.evidence | md)\(adr_note) |"),
      "",
      "\(length) of \($n) rows changed: \(map(select(.delta > 0)) | length) up, \(map(select(.delta < 0)) | length) down."
    end
  elif $part == "regressions" then
    map(select(.delta < 0))
    | if length == 0 then "No row went down from \($a) to \($b)." else
        "| Row | Capability | \($a) | \($b) | Δ | ADR note for \($b) |",
        "|---|---|---|---|---|---|",
        (.[] | "| `\(.id)` | \(.name | md) | \(.old) | \(.new) | \(.delta) | \(
          if .adr then .adr | md elif .delta <= -5 then "**none (gate FAIL)**" else "not needed (drop under 5)" end) |")
      end
  else
        "No silent regression (PLAN-OF-PLANS §7.3, no drop of 5 or more without an adr note): \(
      if any($drops[]; .adr == null) then "FAIL" else "ok" end) (drops of 5 or more: \($drops | length), with an adr note: \(
      [$drops[] | select(.adr != null)] | length))."
  end
        '

# changes FILE A B PART (table|regressions|gate|fails): see CHANGES_JQ.
changes() { jq -r --arg a "$2" --arg b "$3" --arg part "$4" "$CHANGES_JQ" "$1"; }

# compare PLANNED MEASURED (compute_raw output, one version each): the planned versus measured table
# and the index-target gate line. Returns 1 if measured E < planned E − 1.0.
compare() {
    awk -F'\t' '
        function d1(x, s) { s = sprintf("%+.1f", x); return s == "-0.0" ? "+0.0" : s }
        function ix(name, i) {
            printf "| %s | %.1f (#%d) | %.1f (#%d) | %s |\n", name, x[1, i], r[1, i], x[2, i], r[2, i], d1(x[2, i] - x[1, i])
        }
        FNR == 1 { f++ }
        $1 == "fw" && $2 == "id_effect" { for (i = 4; i <= 6; i++) { x[f, i] = $i + 0; r[f, i] = $(i + 3) + 0 } }
        $1 == "kpi" { for (i = 2; i <= 5; i++) k[f, i] = $i + 0 }
        END {
            print "| Index | Planned | Measured | Δ |"
            print "|---|---|---|---|"
            ix("U", 4); ix("E", 5); ix("RT", 6)
            split("≥80|<40|leader|median gap", kpi, "|")
            for (i = 2; i <= 5; i++) printf "| %s | %d | %d | %+d |\n", kpi[i - 1], k[1, i], k[2, i], k[2, i] - k[1, i]
            ok = x[2, 5] >= x[1, 5] - 1.0
            printf "\nIndex target (PLAN-OF-PLANS §7.3, measured E ≥ planned E − 1.0): %s (measured %.1f, planned %.1f).\n", (ok ? "ok" : "FAIL"), x[2, 5], x[1, 5]
            exit !ok
        }' <(printf '%s\n' "$1") <(printf '%s\n' "$2")
}

# The rows measured below their planned score (§5.1) at $v, with the next target they carry over to.
# shellcheck disable=SC2016 # jq program, not shell expansions
MISSES_JQ="$SCORE_JQ"'
[.rows[] | planned($v) as $p | select(.id_effect[$v] < $p)
  | {id, name, m: .id_effect[$v], p: $p,
     next: ([.targets | to_entries[] | select((.key | vkey) > ($v | vkey))] | sort_by(.key | vkey) | first)}]
| if length == 0 then "No row is below its planned score at \($v)." else
    "| Row | Capability | Planned | Measured | Gap | Carry-over |",
    "|---|---|---|---|---|---|",
        (.[] | "| `\(.id)` | \(.name | md) | \(.p) | \(.m) | \(.m - .p) | \(
      if .next then "\(.next.key) target \(.next.value)" else "none: set a later target or record why" end) |")
  end
        '

# misses FILE V: see MISSES_JQ.
misses() { jq -r --arg v "$2" "$MISSES_JQ" "$1"; }

# indices_md P V GROUPS PREV CUR: the report sections on indices, rank movement and group scores.
# GROUPS is "<id>\t<name>\t<w_enterprise>\t<w_runtime>" lines; PREV and CUR are compute_raw output
# for P and V.
indices_md() {
    awk -F'\t' -v p="$1" -v v="$2" '
        function d1(x, s) { s = sprintf("%+.1f", x); return s == "-0.0" ? "+0.0" : s }
        function move(a, b) { return a == b ? "unchanged" : (b < a ? "up " (a - b) : "down " (b - a)) }
        function add(list, item) { return list == "" ? item : list ", " item }
        FNR == 1 { f++ }
        f == 1 { gid[++ng] = $1; gname[$1] = $2; we[$1] = $3 + 0; wr[$1] = $4 + 0; next }
        $1 == "meta" { basis[f] = $2 }
        $1 == "fw" {
            if (f == 2) { fid[++nf] = $2; fname[$2] = $3 }
            for (i = 4; i <= 6; i++) { x[f, $2, i] = $i + 0; r[f, $2, i] = $(i + 3) + 0 }
        }
        $1 == "grp" { gs[f, $2, $3] = $4 + 0 }
        $1 == "kpi" { for (i = 2; i <= 5; i++) k[f, i] = $i + 0 }
        END {
            me = "id_effect"
            print "## Indices before and after"
            print ""
            print "| Version | U | E (rank) | RT (rank) | ≥80 | <40 | leader | median gap |"
            print "|---|---|---|---|---|---|---|---|"
            for (f = 2; f <= 3; f++)
                printf "| %s %s | %.1f | %.1f (#%d) | %.1f (#%d) | %d | %d | %d | %d |\n", (f == 2 ? p : v), basis[f],
                    x[f, me, 4], x[f, me, 5], r[f, me, 5], x[f, me, 6], r[f, me, 6], k[f, 2], k[f, 3], k[f, 4], k[f, 5]
            printf "| Δ | %s | %s | %s | %+d | %+d | %+d | %+d |\n", d1(x[3, me, 4] - x[2, me, 4]), d1(x[3, me, 5] - x[2, me, 5]),
                d1(x[3, me, 6] - x[2, me, 6]), k[3, 2] - k[2, 2], k[3, 3] - k[2, 3], k[3, 4] - k[2, 4], k[3, 5] - k[2, 5]
            print ""
            print "Every Δ in this report is computed from unrounded values (METHOD §5 step 7), so it can differ by 0.1 from the difference of the rounded values shown."

            print ""
            print "## Rank movement"
            print ""
            printf "id_effect among the %d frameworks (1 = best, ties take the better rank).\n\n", nf
            print "| Index | " p " | " v " | Movement | Passed | Fell behind |"
            print "|---|---|---|---|---|---|"
            split("U|E|RT", iname, "|")
            for (i = 4; i <= 6; i++) {
                passed = ""
                behind = ""
                for (j = 2; j <= nf; j++) {
                    c = fid[j]
                    before = x[2, c, i] > x[2, me, i]
                    after = x[3, c, i] > x[3, me, i]
                    if (before && !after) passed = add(passed, sprintf("%s (%.1f)", fname[c], x[3, c, i]))
                    if (!before && after) behind = add(behind, sprintf("%s (%.1f)", fname[c], x[3, c, i]))
                }
                printf "| %s | #%d | #%d | %s | %s | %s |\n", iname[i - 3], r[2, me, i], r[3, me, i], move(r[2, me, i], r[3, me, i]),
                    (passed == "" ? "—" : passed), (behind == "" ? "—" : behind)
            }

            print ""
            print "## Group scores"
            print ""
            print "| Group | Weight E | Weight RT | " p " | " v " | Δ | Best competitor at " v " |"
            print "|---|---|---|---|---|---|---|"
            for (g = 1; g <= ng; g++) {
                id = gid[g]
                bv = -1
                for (j = 2; j <= nf; j++) if (gs[3, fid[j], id] > bv) { bv = gs[3, fid[j], id]; best = fname[fid[j]] }
                printf "| %s | %g | %g | %.1f | %.1f | %s | %s %.1f |\n", gname[id], we[id], wr[id], gs[2, me, id], gs[3, me, id],
                    d1(gs[3, me, id] - gs[2, me, id]), best, bv
            }
        }' <(printf '%s\n' "$3") <(printf '%s\n' "$4") <(printf '%s\n' "$5")
}

# report FILE V P: docs/scorecard/REPORT-V.md (PLAN-OF-PLANS §7.4; METHOD §6 step 6) on stdout.
# shellcheck disable=SC2329 # invoked through generate
report() {
    local file="$1" v="$2" p="$3" groups cur prev planned later bench result rows
    local -a specs=()
    groups="$(jq -r '.groups[] | [.id, .name, .w_enterprise, .w_runtime] | @tsv' "$file")"
    cur="$(compute_raw "$file" "$v" version)"
    prev="$(compute_raw "$file" "$p" version)"
    planned="$(compute_raw "$file" "$v" targets)"
    # shellcheck disable=SC2016 # jq program, not shell expansions
    later="$(jq -r --arg v "$v" "$SCORE_JQ"'target_versions[] | select(vkey > ($v | vkey))' "$file")"

    # shellcheck disable=SC2016 # jq program, not shell expansions
    jq -r --arg v "$v" --arg p "$p" --arg src "${file#"$ROOT"/}" '
        "# Scorecard report: id_effect \($v)",
        "",
        "Generated by `bash scripts/scorecard.sh --report \($v) --prev \($p)` from `\($src)` (METHOD.md v\(.method_version), competitor snapshot \(.competitor_snapshot.date)). Do not edit it by hand: change the data and regenerate. It compares the id_effect scores measured at \($v) with those measured at \($p), in the sections of PLAN-OF-PLANS §7.4.",
        ""' "$file"
    indices_md "$p" "$v" "$groups" "$prev" "$cur"

    printf '\n## Target versus actual\n\n'
    echo "The planned indices for $v (METHOD §5.1: per row, the highest target at or before $v, or the latest measured score if higher) beside the measured ones."
    echo
    compare "$planned" "$cur" || true

    printf '\n## Rows changed\n\n'
    changes "$file" "$p" "$v" table

    printf '\n## Regressions\n\n'
    changes "$file" "$p" "$v" regressions
    echo
    changes "$file" "$p" "$v" gate

    printf '\n## Misses and carry-over\n\n'
    misses "$file" "$v"

    printf '\n## DX KPIs\n\n'
    if [[ -f "$ROOT/scripts/dx-kpis.sh" ]]; then
        echo "Not captured in this report: run \`bash scripts/dx-kpis.sh\` and check the $v row of PLAN-OF-PLANS §6."
    else
        echo "Not yet measured: \`scripts/dx-kpis.sh\` (PLAN-OF-PLANS §8) is not in the tree yet."
    fi

    printf '\n## Bench summary\n\n'
    bench="docs/benchmarks/REPORT-${v%.*}.md"
    if [[ -f "$ROOT/$bench" ]]; then
        result="$(grep -m1 '^\*\*Result:' "$ROOT/$bench" || true)"
        echo "See [\`$bench\`](../benchmarks/${bench##*/}).${result:+ $result}"
    else
        echo "Not yet measured: there is no \`$bench\`."
    fi

    printf '\n## Next release slot\n\n'
    if [[ -z "$later" ]]; then
        echo "No target after $v in \`rows[].targets\`: set the next release's targets (PLAN-OF-PLANS §5)."
        return
    fi
    rows="$(jq --arg s "${later%%$'\n'*}" '[.rows[] | select(.targets[$s] != null)] | length' "$file")"
    echo "The next slot is **${later%%$'\n'*}**, with a target on $rows rows. It stands as planned in \`rows[].targets\`; to adjust it, change the targets and regenerate this report. The planned path from $v (METHOD §5.1, same competitor snapshot):"
    echo
    echo "| Version | U | E (rank) | RT (rank) | ≥80 | <40 | leader | median gap |"
    echo "|---|---|---|---|---|---|---|---|"
    mapfile -t specs <<<"$later"
    id_rows "$file" "version:$v" "${specs[@]/#/targets:}"
}

# The page data: the scorecard fields the page shows, every view (each measured version, then each
# later target version) with its --json result, and each row's id_effect score per view. Numbers
# from scorecard.json are normalised with + 0, so every jq version prints them alike.
# shellcheck disable=SC2016 # jq program, not shell expansions
HTML_JQ="$SCORE_JQ"'
. as $res
| $doc[0] as $d
| [$res[] | {version, basis}] as $views
| {generated_by: "scripts/scorecard.sh --html",
   method_version: $d.method_version,
   competitor_snapshot: $d.competitor_snapshot,
   frameworks: [$d.frameworks[] | {id, name, scope}],
   groups: [$d.groups[] | {id, name, w_enterprise: (.w_enterprise + 0), w_runtime: (.w_runtime + 0)}],
   tracks: $d.tracks,
   views: $views,
   results: ([$res[] | {key: .version, value: .}] | from_entries),
   rows: [$d.rows[] | {id, group, name, measures, track, stance, evidence, competitors, targets,
     adr: (.adr // {}),
     scores: [$views[] as $w
       | if $w.basis == "measured" then .id_effect[$w.version] else planned($w.version) end]}]}
'

# html FILE: docs/scorecard/template.html with its @SCORECARD_DATA@ line replaced by the page data
# (HTML_JQ, with "</" escaped so it cannot close the script element). The template renders it.
# shellcheck disable=SC2329 # invoked through generate
html() {
    local file="$1" tpl="$DIR/template.html" n v data line
    local -a list=()
    [[ -f "$tpl" ]] || { echo "scorecard: no such file: ${tpl#"$ROOT"/}" >&2; exit 2; }
    n="$(grep -c -x '@SCORECARD_DATA@' "$tpl" || true)"
    if [[ "$n" != 1 ]]; then
        echo "scorecard: FAIL ${tpl#"$ROOT"/}: want one @SCORECARD_DATA@ line, found $n" >&2
        return 1
    fi
    : >"$SCRATCH/views.json"
    data="$(versions "$file" measured)"
    mapfile -t list <<<"$data"
    for v in "${list[@]}"; do
        [[ -n "$v" ]] && compute "$file" "$v" version | render_json "$v" >>"$SCRATCH/views.json"
    done
    data="$(versions "$file" target_versions)"
    mapfile -t list <<<"$data"
    for v in "${list[@]}"; do
        [[ -n "$v" ]] && compute "$file" "$v" targets | render_json "$v" >>"$SCRATCH/views.json"
    done
    data="$(jq -c -s --slurpfile doc "$file" "$HTML_JQ" "$SCRATCH/views.json" | sed 's#</#<\\/#g')"
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" == "@SCORECARD_DATA@" ]]; then
            printf '%s\n' "$data"
        else
            printf '%s\n' "$line"
        fi
    done <"$tpl"
}

# The row sections of REPORT-method-v2.md ($part: rows, gaps or caps) from the scorecard, at the
# measured versions $vs: each id_effect row v1 beside v2; every id_effect score and every v1 leader
# (the best competitor in v1, ties to the first in competitor_order) that moves by more than 15;
# every id_effect v1 score above the cap of its v2 level (METHOD §3.2).
# shellcheck disable=SC2016 # jq program, not shell expansions
METHOD_ROWS_JQ="$SCORE_JQ"'
def signed: if . > 0 then "+\(.)" else tostring end;
def v2cell: "\(.v2) (\(.level), \(.met)/\(.n))";
. as $d
| ([$d.frameworks[] | {key: .id, value: .name}] | from_entries) as $names
| [$d.rows[] | . as $r | (.checks | length) as $n | .v2 as $w
    | {id, name, n: $n, notes: ($w.notes // {}),
       ver: [$vs[] as $v | {at: $v, v1: $r.id_effect[$v], v2: v2score($w.level.id_effect[$v]; $w.met.id_effect[$v]; $n),
               level: $w.level.id_effect[$v], met: ($w.met.id_effect[$v] | length), n: $n}],
       leader: (($r.competitors | max) as $top
         | $d.competitor_order[[range(0; 10) | select($r.competitors[.] == $top)] | first] as $f
         | {f: $f, at: "snapshot \($d.competitor_snapshot.date)", v1: $top,
            v2: v2score($w.level[$f]; $w.met[$f]; $n), level: $w.level[$f], met: ($w.met[$f] | length), n: $n})}]
| if $part == "rows" then
    "| Row | Checks | \([$vs[] | "\(.) v1 | \(.) v2 (level, met) | Δ"] | join(" | ")) |",
    "|---|---|\([$vs[] | "---|---|---|"] | join(""))",
    (.[] | "| `\(.id)` | \(.n) | \([.ver[] | "\(.v1) | \(v2cell) | \(.v2 - .v1 | signed)"] | join(" | ")) |")
  elif $part == "gaps" then
    [.[] | . as $row
      | ((.ver[] | {f: "id_effect"} + .), (.leader | {f} + .))
      | select((.v2 - .v1 | fabs) > 15) | . + {id: $row.id, note: $row.notes[.f]}]
    | if length == 0 then "No id_effect score or v1 leader score moves by more than 15." else
        "| Row | Framework | Scored at | v1 | v2 (level, met) | Δ | Explanation |",
        "|---|---|---|---|---|---|---|",
        (.[] | "| `\(.id)` | \($names[.f]) | \(.at) | \(.v1) | \(v2cell) | \(.v2 - .v1 | signed) | \(.note // "No note recorded." | md) |"),
        "",
        "\(length) scores move by more than 15: \(map(select(.f == "id_effect")) | length) of id_effect, \(map(select(.f != "id_effect")) | length) of v1 leaders."
      end
  else
    [.[] | . as $row | .ver[] | select(.v1 > (.level | cap))
      | {id: $row.id, name: $row.name, at, v1, level, cap: (.level | cap), v2}]
    | if length == 0 then "No id_effect v1 score exceeds the cap of its v2 level." else
        "| Row | Capability | Version | v1 | v2 level | Cap | v2 |",
        "|---|---|---|---|---|---|---|",
        (.[] | "| `\(.id)` | \(.name | md) | \(.at) | \(.v1) | \(.level) | \(.cap) | \(.v2) |"),
        "",
        "\(length) v1 scores on \(map(.id) | unique | length) rows exceed the cap of their v2 level."
      end
  end
'

# method_md V1 V2 (compute_raw output for one version, by method 1 and 2): the indices of every
# framework, v1 beside v2, then the id_effect KPIs. Frameworks in the v1 display order.
method_md() {
    awk -F'\t' '
        function d1(x, s) { s = sprintf("%+.1f", x); return s == "-0.0" ? "+0.0" : s }
        FNR == 1 { f++ }
        $1 == "fw" {
            if (f == 1) { pos[$10] = $2; name[$2] = $3; n++ }
            for (i = 4; i <= 6; i++) { x[f, $2, i] = $i + 0; r[f, $2, i] = $(i + 3) + 0 }
        }
        $1 == "kpi" { for (i = 2; i <= 5; i++) k[f, i] = $i + 0 }
        END {
            print "| Framework | U v1 (rank) | U v2 (rank) | Δ U | E v1 (rank) | E v2 (rank) | Δ E | RT v1 (rank) | RT v2 (rank) | Δ RT |"
            print "|---|---|---|---|---|---|---|---|---|---|"
            for (j = 0; j < n; j++) {
                c = pos[j]
                line = "| " name[c]
                for (i = 4; i <= 6; i++)
                    line = line sprintf(" | %.1f (#%d) | %.1f (#%d) | %s", x[1, c, i], r[1, c, i], x[2, c, i], r[2, c, i], d1(x[2, c, i] - x[1, c, i]))
                print line " |"
            }
            print ""
            print "| id_effect KPI | v1 | v2 | Δ |"
            print "|---|---|---|---|"
            split("≥80|<40|leader|median gap", kpi, "|")
            for (i = 2; i <= 5; i++) printf "| %s | %d | %d | %+d |\n", kpi[i - 1], k[1, i], k[2, i], k[2, i] - k[1, i]
        }' <(printf '%s\n' "$1") <(printf '%s\n' "$2")
}

# method_compare FILE: docs/scorecard/REPORT-method-v2.md (METHOD §7) on stdout.
# shellcheck disable=SC2329 # invoked through generate
method_compare() {
    local file="$1" v1 v2 vs v status
    local -a list=()
    v1="$(by_method "$file" 1)"
    v2="$(by_method "$file" 2)"
    if [[ "$(primary_method "$file")" == 2 ]]; then
        status="Since method 2.0 (the re-base of PLAN-OF-PLANS §7.0), v2 is primary and the v1 shown is method 1.1: the v1 scores with the §3.2 caps applied strictly, kept in rows[].v1 as history."
    else
        status="v2 is reported alongside v1; v1 remains primary until the owner accepts this report and the cap violations below are adjudicated."
    fi
    # Versions measured under both methods (the 1.1 history stops at 0.7.0).
    vs="$(jq -c "$SCORE_JQ"'measured' "$v1")"
    mapfile -t list < <(jq -r '.[]' <<<"$vs")

    # shellcheck disable=SC2016 # jq program, not shell expansions
    jq -r --arg src "${file#"$ROOT"/}" --arg status "$status" '
        "# Scorecard method v2 beside v1",
        "",
        "Generated by `bash scripts/scorecard.sh --method-compare` from `\($src)` (METHOD.md v\(.method_version) §7, competitor snapshot \(.competitor_snapshot.date)). Do not edit it by hand: change the data and regenerate.",
        "",
        "v1 is anchored judgment with the hard caps of METHOD §3.2. v2 scores each row from its 4 to 8 table-stakes checks: s = clamp(1, cap(level), round(100 × met / checks)), where round is half away from zero and cap(level) is absent 9, labs 39, published 49, tested 69, documented 89, production 100. Both go through the same §5 index pipeline. \($status)",
        "",
        "Every Δ is v2 − v1. Index Δs are computed from unrounded values (METHOD §5 step 7), so they can differ by 0.1 from the difference of the rounded values shown."' "$file"
    for v in "${list[@]}"; do
        printf '\n## Indices at %s\n\n' "$v"
        method_md "$(compute_raw "$v1" "$v" version)" "$(compute_raw "$v2" "$v" version)"
    done

    printf '\n## id_effect rows, v1 beside v2\n\n'
    jq -r --argjson vs "$vs" --arg part rows "$METHOD_ROWS_JQ" "$v1"

    printf '\n## Rows where |v1 − v2| > 15\n\n'
    echo "Each id_effect score, and the score of each row's v1 leader (the best competitor in v1; ties go to the first in \`competitor_order\`), whose v2 score differs from v1 by more than 15. The explanation is the row's \`v2.notes\` entry for that framework."
    echo
    jq -r --argjson vs "$vs" --arg part gaps "$METHOD_ROWS_JQ" "$v1"

    printf '\n## v1 cap violations\n\n'
    echo "Each id_effect v1 score above the cap that METHOD §3.2 sets for its v2 level. Method 1.1 lowers every such score to its cap, so from 1.1 on this list is empty."
    echo
    jq -r --argjson vs "$vs" --arg part caps "$METHOD_ROWS_JQ" "$v1"
}

# generate DEST CMD...: run CMD into the scratch directory, then copy the result over DEST whole.
generate() {
    local dest="$1"
    shift
    "$@" >"$SCRATCH/generated"
    cat "$SCRATCH/generated" >"$dest"
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

    # METHOD.md §5.2, copied verbatim: method 2.0, then the method 1.1 history (rows[].v1).
    read -r -d '' want <<'EOF' || true
| 0.6.0 measured | 38.5 | 37.3 (#10) | 46.0 (#6) | 4 | 47 | 2 | 62 |
| 0.7.0 measured | 45.3 | 43.2 (#9) | 56.1 (#6) | 13 | 39 | 4 | 50 |
| 0.8.0 measured | 56.0 | 55.2 (#5) | 64.4 (#2) | 28 | 31 | 7 | 25 |
| 0.9.0 measured | 67.2 | 67.2 (#3) | 70.8 (#2) | 38 | 21 | 19 | 12 |
| 0.10.0 target | 73.5 | 74.1 (#2) | 77.0 (#1) | 44 | 11 | 20 | 12 |
| 0.11.0 target | 77.2 | 77.2 (#2) | 81.3 (#1) | 52 | 8 | 26 | 12 |
| 0.12.0 target | 80.7 | 80.9 (#1) | 83.3 (#1) | 53 | 3 | 26 | 12 |
| 1.0.0 target | 83.5 | 83.6 (#1) | 85.9 (#1) | 59 | 1 | 30 | 12 |
EOF
    got="$(id_rows "$file" version:0.6.0 version:0.7.0)"
    got+=$'\n'"$(id_rows "$file" version:0.8.0 version:0.9.0 targets:0.10.0 targets:0.11.0)"
    got+=$'\n'"$(id_rows "$file" targets:0.12.0 targets:1.0.0)"
    expect "METHOD §5.2 version table" "$want" "$got"

    read -r -d '' want <<'EOF' || true
| .NET | 75.1 | 77.3 | 71.1 |
| Spring | 70.1 | 73.0 | 63.8 |
| Phoenix | 60.3 | 59.6 | 59.5 |
| Effect-TS | 56.8 | 56.0 | 60.4 |
| ZIO | 54.2 | 51.8 | 56.6 |
| Laravel | 52.2 | 53.3 | 42.3 |
| Rails | 51.3 | 51.5 | 40.6 |
| Django | 45.5 | 45.6 | 36.7 |
| Next.js | 38.9 | 41.0 | 33.4 |
| OTP | 37.3 | 36.6 | 43.2 |
EOF
    expect "METHOD §5.2 competitor table" "$want" "$(compute "$file" 0.7.0 version | render 0.7.0 competitors)"

    read -r -d '' want <<'EOF' || true
| 0.6.0 measured | 44.2 | 42.6 (#11) | 53.3 (#9) | 9 | 36 | 2 | 47 |
| 0.7.0 measured | 47.7 | 45.6 (#11) | 57.8 (#7) | 16 | 34 | 3 | 40 |
| 0.8.0 target | 55.0 | 53.7 (#11) | 63.7 (#7) | 25 | 26 | 4 | 28 |
| 0.9.0 target | 62.5 | 62.2 (#9) | 67.6 (#6) | 35 | 20 | 4 | 20 |
| 0.10.0 target | 69.3 | 69.2 (#5) | 73.8 (#4) | 41 | 11 | 6 | 15 |
| 0.11.0 target | 73.0 | 72.2 (#4) | 78.4 (#2) | 50 | 8 | 8 | 12 |
| 0.12.0 target | 76.6 | 76.4 (#4) | 80.7 (#1) | 51 | 3 | 8 | 12 |
| 1.0.0 target | 80.3 | 80.3 (#3) | 83.3 (#1) | 59 | 1 | 10 | 11 |
EOF
    got="$(historic "$file")"
    got="$(id_rows "$got" version:0.6.0 version:0.7.0 targets:0.8.0 targets:0.9.0 targets:0.10.0 \
        targets:0.11.0 targets:0.12.0 targets:1.0.0)"
    expect "METHOD §5.2 method 1.1 history table" "$want" "$got"

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
bad-adr adr key "next" is not x.y.z
bad-v2-check-ref v2.met.a has unknown check id "core.a#9"
bad-v2-level v2.level.b = "beta" is not a level
bad-v2-stored competitors[2] = 10 is not its checklist score 9
bad-process-level process row: v2.level.b = "documented", want "production"
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

    # --diff 0.1.0 0.2.0: core.a drops 10 (its adr note is for 0.1.0), core.b 4 (under the gate),
    # core.c exactly 5 (the boundary) and core.d rises 10. regression-with-adr.json adds adr notes
    # for 0.2.0 to core.a and core.c, which turns the FAIL into ok.
    read -r -d '' want <<'EOF' || true
| Row | Capability | 0.1.0 | 0.2.0 | Δ | Evidence |
|---|---|---|---|---|---|
| `core.a` | Row A | 50 | 40 | -10 | fixture |
| `core.b` | Row B | 50 | 46 | -4 | fixture |
| `core.c` | Row C | 50 | 45 | -5 | fixture |
| `core.d` | Row D | 50 | 60 | +10 | fixture |

4 of 5 rows changed: 1 up, 3 down.
EOF
    expect "fixture valid/regression --diff table" "$want" "$(changes "$FIXTURES/valid/regression.json" 0.1.0 0.2.0 table)"
    read -r -d '' want <<'EOF' || true
row "core.a": 50 at 0.1.0, 40 at 0.2.0 (-10) with no adr note for 0.2.0
row "core.c": 50 at 0.1.0, 45 at 0.2.0 (-5) with no adr note for 0.2.0
EOF
    expect "fixture valid/regression --diff FAILs" "$want" "$(changes "$FIXTURES/valid/regression.json" 0.1.0 0.2.0 fails)"
    expect "fixture valid/regression-with-adr --diff FAILs" "" \
        "$(changes "$FIXTURES/valid/regression-with-adr.json" 0.1.0 0.2.0 fails)"
    while read -r name want; do
        file="$FIXTURES/valid/$name.json"
        set +e
        out="$(bash "${BASH_SOURCE[0]}" --file "$file" --validate --diff 0.1.0 0.2.0 2>&1)"
        rc=$?
        set -e
        if [[ "$rc" == "$want" ]]; then
            echo "self-test: ok   fixture valid/$name --diff 0.1.0 0.2.0 (exit $rc)"
        else
            echo "self-test: FAIL fixture valid/$name --diff 0.1.0 0.2.0 (exit $rc, want $want)"
            printf '%s\n' "$out" | sed 's/^/    /'
            failed=1
        fi
    done <<'EOF'
regression 1
regression-with-adr 0
EOF

    # Method 2 (METHOD §7): the row scores of valid/v2.json (see its measures) give id_effect
    # 13 and 39, U 26.0; competitors e to j average 81.5, a 31.5 and c 42.0 are ahead too.
    file="$FIXTURES/valid/v2.json"
    if out="$(check "$file" 2>&1)"; then
        echo "self-test: ok   validate fixture valid/v2"
    else
        echo "self-test: FAIL validate fixture valid/v2"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=1
    fi
    # shellcheck disable=SC2016 # jq program, not shell expansions
    expect "fixture valid/v2 method 2 row scores" "13 38 69 9 1 63 63 63 63 63 63 | 39 25 89 75 50 100 100 100 100 100 100" \
        "$(jq -r "$SCORE_JQ"'v2doc | [.rows[] | [.id_effect["0.1.0"]] + .competitors | map(tostring) | join(" ")] | join(" | ")' "$file")"
    expect "fixture valid/v2 method 2 indices" "| 0.1.0 measured | 26.0 | 26.0 (#10) | 26.0 (#10) | 0 | 2 | 0 | 61 |" \
        "$(id_rows "$(rebase "$file")" version:0.1.0)"

    # Method 2.0 (METHOD §7.4): valid/v2-primary.json is valid/v2.json with the checklist scores
    # stored and the method 1 scores kept in rows[].v1. Its own method gives the method 2 indices
    # above; --method 1 gives back those of valid/v2.json. bad-v2-stored (above) changes one score.
    file="$FIXTURES/valid/v2-primary.json"
    if out="$(check "$file" 2>&1)"; then
        echo "self-test: ok   validate fixture valid/v2-primary"
    else
        echo "self-test: FAIL validate fixture valid/v2-primary"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=1
    fi
    expect "fixture valid/v2-primary indices" "| 0.1.0 measured | 26.0 | 26.0 (#10) | 26.0 (#10) | 0 | 2 | 0 | 61 |" \
        "$(id_rows "$file" version:0.1.0)"
    expect "fixture valid/v2-primary --method 1 indices" "$(id_rows "$FIXTURES/valid/v2.json" version:0.1.0)" \
        "$(id_rows "$(historic "$file")" version:0.1.0)"

    # --html and --report must be byte-identical across runs (CI diffs the committed files).
    file="$DIR/scorecard.json"
    html "$file" >"$SCRATCH/a.html"
    html "$file" >"$SCRATCH/b.html"
    if cmp -s "$SCRATCH/a.html" "$SCRATCH/b.html" && ! grep -q -x '@SCORECARD_DATA@' "$SCRATCH/a.html"; then
        echo "self-test: ok   --html deterministic"
    else
        echo "self-test: FAIL --html differs between two runs, or kept its @SCORECARD_DATA@ line"
        failed=1
    fi
    report "$file" 0.7.0 0.6.0 >"$SCRATCH/a.md"
    report "$file" 0.7.0 0.6.0 >"$SCRATCH/b.md"
    if cmp -s "$SCRATCH/a.md" "$SCRATCH/b.md"; then
        echo "self-test: ok   --report 0.7.0 deterministic"
    else
        echo "self-test: FAIL --report 0.7.0 differs between two runs"
        failed=1
    fi
    method_compare "$file" >"$SCRATCH/a-v2.md"
    method_compare "$file" >"$SCRATCH/b-v2.md"
    if cmp -s "$SCRATCH/a-v2.md" "$SCRATCH/b-v2.md"; then
        echo "self-test: ok   --method-compare deterministic"
    else
        echo "self-test: FAIL --method-compare differs between two runs"
        failed=1
    fi

    ((failed == 0)) || { echo "scorecard: self-test FAILED"; exit 1; }
    echo "scorecard: self-test passed"
}

file="$DIR/scorecard.json"
write=0
prev=""
method=""
actions=()
while (($#)); do
    case "$1" in
        --validate | --self-test | --html | --method-compare) actions+=("${1#--}"); shift ;;
        --method)
            [[ "${2:-}" =~ ^[12]$ ]] || { echo "scorecard: --method needs 1 or 2" >&2; exit 2; }
            method="$2"
            shift 2
            ;;
        --version | --targets | --json | --compare-targets | --report | --prev)
            [[ "${2:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "scorecard: $1 needs a version x.y.z" >&2; exit 2; }
            if [[ "$1" == --prev ]]; then prev="$2"; else actions+=("${1#--}:$2"); fi
            shift 2
            ;;
        --diff)
            [[ "${2:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && "${3:-}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
                || { echo "scorecard: --diff needs two versions x.y.z" >&2; exit 2; }
            actions+=("diff:$2:$3")
            shift 3
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
if [[ -n "$prev" && " ${actions[*]} " != *" report:"* ]]; then
    echo "scorecard: --prev needs --report" >&2
    exit 2
fi
# The file's own method (METHOD §7.4) unless --method asks for the other one, which works with
# --version, --json and --diff (and --targets for method 1 on a method 2 file) only.
primary=1
[[ -f "$file" ]] && primary="$(primary_method "$file" 2>/dev/null || echo 1)"
method="${method:-$primary}"
if [[ "$method" != "$primary" ]]; then
    for action in "${actions[@]}"; do
        allowed=0
        case "$action" in
            validate | version:* | json:* | diff:*) allowed=1 ;;
            targets:*) [[ "$method" == 1 ]] && allowed=1 ;;
        esac
        ((allowed)) || {
            echo "scorecard: --method $method on a method $primary scorecard works with --version, --json and --diff only" >&2
            exit 2
        }
    done
    ((write == 0)) || { echo "scorecard: --write stores results of the file's own method only" >&2; exit 2; }
fi

# --diff and --compare-targets report every result before a missed gate makes the exit status 1.
status=0
for action in "${actions[@]}"; do
    case "$action" in
        validate)
            check "$file"
            echo "scorecard: ${file#"$ROOT"/} valid ($(jq '.rows | length' "$file") rows)"
            ;;
        self-test) self_test ;;
        version:* | targets:*)
            check "$file"
            [[ "$method" == 2 && "$primary" == 1 ]] && require_measured "$file" "${action#*:}"
            scored="$(by_method "$file" "$method")"
            compute "$scored" "${action#*:}" "${action%%:*}" | render "${action#*:}" all
            ;;
        json:*)
            check "$file"
            [[ "$method" == 2 && "$primary" == 1 ]] && require_measured "$file" "${action#*:}"
            scored="$(by_method "$file" "$method")"
            out="$(compute "$scored" "${action#*:}" version | render_json "${action#*:}")"
            if ((write)); then
                mkdir -p "$DIR/results"
                printf '%s\n' "$out" >"$DIR/results/v${action#*:}.json"
                echo "scorecard: wrote docs/scorecard/results/v${action#*:}.json"
            else
                printf '%s\n' "$out"
            fi
            ;;
        diff:*)
            check "$file"
            a="${action#diff:}"
            b="${a#*:}"
            a="${a%%:*}"
            require_measured "$file" "$a"
            require_measured "$file" "$b"
            scored="$(by_method "$file" "$method")"
            kind="measured id_effect scores"
            [[ "$method" == "$primary" ]] || kind+=" by method $method"
            printf 'scorecard: diff %s → %s (%s, %s)\n\n' "$a" "$b" "$kind" "${file#"$ROOT"/}"
            changes "$scored" "$a" "$b" table
            echo
            changes "$scored" "$a" "$b" gate
            out="$(changes "$scored" "$a" "$b" fails)"
            if [[ -n "$out" ]]; then
                while IFS= read -r line; do echo "scorecard: FAIL ${file#"$ROOT"/}: $line" >&2; done <<<"$out"
                status=1
            fi
            ;;
        compare-targets:*)
            v="${action#*:}"
            check "$file"
            require_measured "$file" "$v"
            planned="$(compute_raw "$file" "$v" targets)"
            measured="$(compute_raw "$file" "$v" version)"
            printf 'scorecard: %s measured vs planned (METHOD §5.1, %s)\n\n' "$v" "${file#"$ROOT"/}"
            if ! compare "$planned" "$measured"; then
                echo "scorecard: FAIL ${file#"$ROOT"/}: measured E at $v is more than 1.0 below planned E" >&2
                status=1
            fi
            echo
            misses "$file" "$v"
            ;;
        report:*)
            v="${action#*:}"
            check "$file"
            require_measured "$file" "$v"
            p="$prev"
            if [[ -z "$p" ]]; then
                # shellcheck disable=SC2016 # jq program, not shell expansions
                p="$(jq -r --arg v "$v" "$SCORE_JQ"'[measured[] | select(vkey < ($v | vkey))] | last // empty' "$file")"
                [[ -n "$p" ]] || { echo "scorecard: no measured version before $v; pass --prev" >&2; exit 2; }
            fi
            require_measured "$file" "$p"
            generate "$DIR/REPORT-$v.md" report "$file" "$v" "$p"
            echo "scorecard: wrote docs/scorecard/REPORT-$v.md ($p → $v)"
            ;;
        html)
            check "$file"
            generate "$DIR/index.html" html "$file"
            echo "scorecard: wrote docs/scorecard/index.html"
            ;;
        method-compare)
            check "$file"
            generate "$DIR/REPORT-method-v2.md" method_compare "$file"
            echo "scorecard: wrote docs/scorecard/REPORT-method-v2.md"
            ;;
    esac
done
exit "$status"
