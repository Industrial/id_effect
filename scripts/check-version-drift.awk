# Used by scripts/check-version-drift.sh (kept separate so shell formatters never parse awk).
# Vars: want (workspace version), file (manifest path).
# Prints one line per stale id_effect* pin and a final "#checked N" line; exits 1 if any is stale.
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
