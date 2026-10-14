# Core of spikes/scripts/count-annotations.sh (POSIX awk; no gawk extensions).
#
# Variables (set with -v):
#   corpus_ids  space-separated expected region ids, in report order (may be empty)
#   json        1 for JSON output, else TSV
#
# Counting rules are documented in spikes/corpus/README.md ("What counts as an annotation").
# Signature lines (fn ... up to the opening `{`) are never counted: they are declarations both
# designs need. Only expressions and statements in bodies are.

function strip(s) {
  gsub(/\\\\/, "", s)
  gsub(/\\"/, "", s)
  gsub(/"[^"]*"/, "\"\"", s)
  gsub(/'[^'\\]'/, "' '", s)
  gsub(/\/\*[^*]*\*\//, "", s)
  sub(/\/\/.*/, "", s)
  return s
}

function count(s, re,    n) {
  n = 0
  while (match(s, re)) {
    n++
    s = substr(s, RSTART + RLENGTH)
  }
  return n
}

function fwd_items(s,    n, inner, parts, i, k, item) {
  n = 0
  while (match(s, /forward[ \t]*\([^)]*\)/)) {
    inner = substr(s, RSTART, RLENGTH)
    s = substr(s, RSTART + RLENGTH)
    sub(/^forward[ \t]*\(/, "", inner)
    sub(/\)$/, "", inner)
    k = split(inner, parts, ",")
    for (i = 1; i <= k; i++) {
      item = parts[i]
      gsub(/[ \t]/, "", item)
      if (item != "") n++
    }
  }
  return n
}

function add(key, tf, lt, ct, mk, ix, fw) {
  c_tf[key] += tf
  c_lt[key] += lt
  c_ct[key] += ct
  c_mk[key] += mk
  c_ix[key] += ix
  c_fw[key] += fw
}

function fail(msg) {
  printf "count-annotations: %s:%d: %s\n", FILENAME, FNR, msg > "/dev/stderr"
  errors++
}

function close_file_check() {
  if (cur != "") {
    printf "count-annotations: %s: region %s is never closed\n", prev_file, cur > "/dev/stderr"
    errors++
    cur = ""
  }
  in_sig = 0
}

function jstr(s) {
  gsub(/\\/, "\\\\", s)
  gsub(/"/, "\\\"", s)
  return "\"" s "\""
}

BEGIN {
  n_expected = split(corpus_ids, expected, " ")
  for (i = 1; i <= n_expected; i++) is_expected[expected[i]] = 1
  n_seen = 0
  n_files = 0
  markers = 0
  cur = ""
  errors = 0
}

FNR == 1 {
  if (n_files > 0) close_file_check()
  n_files++
  files[n_files] = FILENAME
  prev_file = FILENAME
}

{
  raw = $0

  if (match(raw, /^[ \t]*\/\/[ \t]*corpus:begin[ \t]+[^ \t]+/)) {
    id = substr(raw, RSTART, RLENGTH)
    sub(/^[ \t]*\/\/[ \t]*corpus:begin[ \t]+/, "", id)
    markers++
    if (cur != "") fail("region " id " opened inside region " cur)
    else if (id in status) fail("region " id " declared twice")
    else {
      cur = id
      status[id] = "ok"
      seen[++n_seen] = id
      add(id, 0, 0, 0, 0, 0, 0)
    }
    in_sig = 0
    next
  }

  if (match(raw, /^[ \t]*\/\/[ \t]*corpus:end[ \t]+[^ \t]+/)) {
    id = substr(raw, RSTART, RLENGTH)
    sub(/^[ \t]*\/\/[ \t]*corpus:end[ \t]+/, "", id)
    if (cur != id) fail("corpus:end " id " does not close the open region '" cur "'")
    cur = ""
    in_sig = 0
    next
  }

  if (match(raw, /^[ \t]*\/\/[ \t]*corpus:unsupported[ \t]+[^ \t]+/)) {
    id = substr(raw, RSTART, RLENGTH)
    sub(/^[ \t]*\/\/[ \t]*corpus:unsupported[ \t]+/, "", id)
    reason_text = substr(raw, RSTART + RLENGTH)
    sub(/^[ \t]+/, "", reason_text)
    markers++
    if (id in status) fail("region " id " declared twice")
    else {
      status[id] = "unsupported"
      reason[id] = reason_text
      seen[++n_seen] = id
      add(id, 0, 0, 0, 0, 0, 0)
    }
    next
  }

  key = (cur != "") ? cur : ("file:" FILENAME)
  mk = (raw ~ /\/\/[ \t]*ann:/) ? 1 : 0
  s = strip(raw)
  fw = fwd_items(s)

  # Signature handling: from a `fn` header up to its opening brace nothing is counted.
  if (!in_sig && s ~ /^[ \t]*(pub(\([^)]*\))?[ \t]+)?((const|async|unsafe|extern)[ \t]+)*fn[ \t]/) in_sig = 1
  if (in_sig) {
    if (index(s, "{") > 0) {
      s = substr(s, index(s, "{") + 1)
      in_sig = 0
    } else {
      if (index(s, ";") > 0) in_sig = 0
      add(key, 0, 0, 0, mk, 0, fw)
      next
    }
  }

  tf = count(s, "::<")
  ix = count(s, "(^|[^A-Za-z0-9_])Here([^A-Za-z0-9_]|$)") + count(s, "There[ \t]*<")
  m = s
  gsub(/::/, "##", m)
  lt = count(m, "(^|[^A-Za-z0-9_])let[ \t]+(mut[ \t]+)?([A-Za-z_][A-Za-z0-9_]*|\\([^)]*\\))[ \t]*:")
  ct = count(m, "\\|[^|{};=.]*:[^|{};=.]*\\|")
  add(key, tf, lt, ct, mk, ix, fw)
}

END {
  if (n_files > 0) close_file_check()

  n_rows = 0
  if (markers == 0) {
    for (i = 1; i <= n_files; i++) {
      id = files[i]
      rows[++n_rows] = id
      rkey[id] = "file:" id
      status[id] = "ok"
    }
  } else {
    for (i = 1; i <= n_expected; i++) {
      id = expected[i]
      rows[++n_rows] = id
      rkey[id] = id
      if (!(id in status)) status[id] = "missing"
    }
    for (i = 1; i <= n_seen; i++) {
      id = seen[i]
      if (!(id in is_expected)) {
        rows[++n_rows] = id
        rkey[id] = id
      }
    }
  }

  ported = 0; unsupported = 0; missing = 0; annotated = 0
  sum_total = 0; sum_ix = 0; sum_fw = 0
  for (i = 1; i <= n_rows; i++) {
    id = rows[i]
    k = rkey[id]
    total[id] = c_tf[k] + c_lt[k] + c_ct[k] + c_mk[k]
    if (status[id] == "ok") {
      ported++
      if (total[id] > 0) annotated++
      sum_total += total[id]
      sum_ix += c_ix[k]
      sum_fw += c_fw[k]
    } else if (status[id] == "unsupported") unsupported++
    else missing++
  }
  # Forward lists declared outside every region (e.g. a shared app env) still count.
  if (markers > 0)
    for (i = 1; i <= n_files; i++) sum_fw += c_fw["file:" files[i]]
  pct = (ported > 0) ? sprintf("%.1f", 100 * annotated / ported) : "0.0"

  if (json == 1) {
    printf "{\n  \"files\": ["
    for (i = 1; i <= n_files; i++) printf "%s%s", (i > 1 ? ", " : ""), jstr(files[i])
    printf "],\n  \"handlers\": [\n"
    for (i = 1; i <= n_rows; i++) {
      id = rows[i]
      k = rkey[id]
      printf "    {\"id\": %s, \"status\": %s", jstr(id), jstr(status[id])
      if (status[id] == "ok")
        printf ", \"turbofish\": %d, \"let_ty\": %d, \"closure_ty\": %d, \"marker\": %d, \"total\": %d, \"index\": %d, \"fwd\": %d", c_tf[k], c_lt[k], c_ct[k], c_mk[k], total[id], c_ix[k], c_fw[k]
      if (status[id] == "unsupported") printf ", \"reason\": %s", jstr(reason[id])
      printf "}%s\n", (i < n_rows ? "," : "")
    }
    printf "  ],\n  \"summary\": {\"handlers\": %d, \"ported\": %d, \"unsupported\": %d, \"missing\": %d, \"annotated\": %d, \"annotated_pct\": %s, \"total\": %d, \"index\": %d, \"fwd\": %d}\n}\n", n_rows, ported, unsupported, missing, annotated, pct, sum_total, sum_ix, sum_fw
  } else {
    printf "id\tstatus\tturbofish\tlet_ty\tclosure_ty\tmarker\ttotal\tindex\tfwd\n"
    for (i = 1; i <= n_rows; i++) {
      id = rows[i]
      k = rkey[id]
      if (status[id] == "ok")
        printf "%s\tok\t%d\t%d\t%d\t%d\t%d\t%d\t%d\n", id, c_tf[k], c_lt[k], c_ct[k], c_mk[k], total[id], c_ix[k], c_fw[k]
      else if (status[id] == "unsupported")
        printf "%s\tunsupported\t-\t-\t-\t-\t-\t-\t-\t%s\n", id, reason[id]
      else
        printf "%s\tmissing\t-\t-\t-\t-\t-\t-\t-\n", id
    }
    printf "summary\thandlers=%d ported=%d unsupported=%d missing=%d annotated=%d annotated_pct=%s total=%d index=%d fwd=%d\n", n_rows, ported, unsupported, missing, annotated, pct, sum_total, sum_ix, sum_fw
  }
  exit (errors > 0 ? 1 : 0)
}
