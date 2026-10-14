#!/usr/bin/env bash
# Fail when a template or example pins a stale id_effect version.
#
# Scans every Cargo.toml (and Cargo.toml.template) under examples/ and crates/*/templates/ and
# checks each `id_effect*` dependency that declares `version = "X"` (inline table, plain string or
# `[dependencies.id_effect_x]` table form). X must equal [workspace.package].version, optionally
# prefixed by `=` or `^`. Workspace-inherited (`workspace = true`) and path-only deps are fine.
#
# Usage: bash scripts/check-version-drift.sh
# Requires: bash, awk, find.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ws_version="$(awk '
  /^\[/ { in_pkg = ($0 ~ /^\[workspace\.package\][[:space:]]*$/); next }
  in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
    if (match($0, /"[^"]*"/)) { print substr($0, RSTART + 1, RLENGTH - 2); exit }
}
' Cargo.toml)"
if [[ -z "$ws_version" ]]; then
    echo "check-version-drift: no [workspace.package].version in Cargo.toml" >&2
    exit 2
fi

mapfile -t manifests < <(
    {
        find examples -type f \( -name Cargo.toml -o -name Cargo.toml.template \) 2>/dev/null || true
        for d in crates/*/templates; do
            if [[ -d "$d" ]]; then
                find "$d" -type f \( -name Cargo.toml -o -name Cargo.toml.template \)
            fi
        done
    } | { grep -v '/target/' || true; } | sort
)

# Prints one line per stale pin and a final "#checked N" line; exits 1 if any pin is stale.
read -r -d '' AWK_CHECK <<'AWK' || true
function check(dep, ver, line) {
  checked++
  sub(/^[=^]/, "", ver)
  if (ver ~ /\{\{/) return # template placeholder
  if (ver != want) {
    printf "%s:%d: %s pins version \"%s\" (workspace is %s)\n", file, line, dep, ver, want
    bad++
  }
}
function version_in(s,   m) {
  if (match(s, /version[[:space:]]*=[[:space:]]*"[^"]*"/)) {
    m = substr(s, RSTART, RLENGTH)
    sub(/^[^"]*"/, "", m)
    sub(/"$/, "", m)
    return m
  }
  return ""
}
BEGIN { bad = 0; checked = 0; depth = 0 }
{
  line = $0
  sub(/#.*/, "", line)
  if (depth > 0) { # continuation of a multi-line inline table
    acc = acc " " line
    depth += gsub(/\{/, "{", line) - gsub(/\}/, "}", line)
    if (depth <= 0) {
      v = version_in(acc)
      if (v != "") check(acc_dep, v, acc_line)
      depth = 0
    }
    next
  }
  if (line ~ /^[[:space:]]*\[/) {
    table_dep = ""
    if (match(line, /dependencies\.id_effect[A-Za-z0-9_-]*\]/)) {
      table_dep = substr(line, RSTART + 13, RLENGTH - 14)
    }
    in_deps = (line ~ /dependencies\][[:space:]]*$/)
    next
  }
  if (table_dep != "" && line ~ /^[[:space:]]*version[[:space:]]*=/) {
    check(table_dep, version_in(line), NR)
    next
  }
  if (in_deps && match(line, /^[[:space:]]*id_effect[A-Za-z0-9_-]*[[:space:]]*=/)) {
    dep = substr(line, RSTART, RLENGTH)
    sub(/=$/, "", dep)
    gsub(/[[:space:]]/, "", dep)
    rest = substr(line, RSTART + RLENGTH)
    if (rest ~ /^[[:space:]]*"/) {
      v = rest
      sub(/^[[:space:]]*"/, "", v)
      sub(/".*/, "", v)
      check(dep, v, NR)
    } else if (rest ~ /^[[:space:]]*\{/) {
      depth = gsub(/\{/, "{", rest) - gsub(/\}/, "}", rest)
      if (depth > 0) {
        acc = rest; acc_dep = dep; acc_line = NR
      } else {
        v = version_in(rest)
        if (v != "") check(dep, v, NR)
      }
    }
  }
}
END { printf "#checked %d\n", checked; exit (bad > 0 ? 1 : 0) }
AWK

drift=0
checked=0
for manifest in "${manifests[@]}"; do
    status=0
    out="$(awk -v want="$ws_version" -v file="$manifest" "$AWK_CHECK" "$manifest")" || status=$?
    n="$(sed -n 's/^#checked //p' <<<"$out")"
    checked=$((checked + ${n:-0}))
    grep -v '^#checked ' <<<"$out" || true
    if ((status != 0)); then
        drift=$((drift + 1))
    fi
done

if ((drift > 0)); then
    echo "check-version-drift: $drift manifest(s) pin an id_effect version other than $ws_version" >&2
    exit 1
fi
echo "check-version-drift: ${#manifests[@]} manifest(s), $checked pinned id_effect dep(s), all at $ws_version"
