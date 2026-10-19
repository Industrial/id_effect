#!/usr/bin/env bash
# Every opt-out records itself (ADR 0013 R10). Fails when a function named `dangerous_*`,
# `insecure_*`, `*_dangerous` or `*_insecure` (the codebase uses both the prefix and the suffix
# form) under crates/ does not call `record_dangerous` (id_effect::observability::posture), the
# hook the prod posture check reads.
#
# A function passes when its body calls `record_dangerous(` itself, or delegates to another
# function of that family (`self.ssrf.dangerous_allow_private_networks()`, which this script
# checks in turn). Not checked: tests (`#[cfg(test)]` modules, `#[test]` functions, the tests/,
# benches/ and examples/ directories), trait method declarations without a body, and the
# functions in EXEMPT below (readers of the registry, not opt-outs). Strings and `//` comments
# are ignored when matching braces and calls.
#
# Usage: bash scripts/check-dangerous.sh [--self-test] [DIR...]
#   no DIR: crates/. --self-test runs the cases below against fixtures in a temporary directory.
# Requires: bash, find, a POSIX awk (AWK=… picks another; gawk, mawk and busybox awk all work).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# `path-suffix:function` pairs that are not opt-outs.
EXEMPT=(
    # The registry itself: the hook, and the list of recorded opt-outs.
    "crates/id_effect/src/observability/posture.rs:record_dangerous"
    "crates/id_effect/src/observability/posture.rs:dangerous_opt_outs"
)

# Reads every file given; prints `file:line: problem` and `checked <n> <direct> <delegated>`.
AWK_PROG="$(
    cat <<'AWK'
# The code of `s` without string contents, char literals and a trailing `//` comment. `in_str`
# carries an unterminated string (a `\`-continued literal) to the next line.
function strip(s,    out, i, c, n) {
    out = ""; n = length(s); i = 1
    while (i <= n) {
        c = substr(s, i, 1)
        if (in_str) {
            if (c == "\\") { i += 2; continue }
            if (c == "\"") { in_str = 0; out = out "\"" }
            i++; continue
        }
        if (c == "/" && substr(s, i + 1, 1) == "/") break
        if (c == "\"") { in_str = 1; out = out "\""; i++; continue }
        if (c == "'") {
            if (substr(s, i + 2, 1) == "'") { i += 3; continue }
            if (substr(s, i + 1, 1) == "\\" && substr(s, i + 3, 1) == "'") { i += 4; continue }
        }
        out = out c; i++
    }
    return out
}
# Whether `name` in `file` is an EXEMPT entry (`path-suffix:function`).
function exempt(file, name,    k, sep, path) {
    for (k = 1; k <= n_ex; k++) {
        sep = index(ex[k], ":")
        path = substr(ex[k], 1, sep - 1)
        if (substr(ex[k], sep + 1) == name && length(file) >= length(path) &&
            substr(file, length(file) - length(path) + 1) == path) return 1
    }
    return 0
}
# Brace depth change of `code`; sets `opened` when a `{` occurs and `semi_first` when a `;`
# comes before any `{` at depth 0.
function braces(code,    i, c, d) {
    d = 0
    for (i = 1; i <= length(code); i++) {
        c = substr(code, i, 1)
        if (c == "{") { d++; opened = 1 }
        else if (c == "}") d--
        else if (c == ";" && !opened && depth + d == 0) semi_first = 1
    }
    return d
}
function finish(    body) {
    body = fn_body
    # The function's own name is not a delegation.
    sub("fn[ \t]+" fn_name, "", body)
    checked++
    if (body ~ /record_dangerous[ \t]*\(/) direct++
    else if (body ~ /(dangerous|insecure)_[A-Za-z0-9_]*[ \t]*(::<[^>]*>)?[ \t]*\(/ || body ~ /[A-Za-z0-9]_(dangerous|insecure)[ \t]*(::<[^>]*>)?[ \t]*\(/) delegated++
    else { printf "%s:%d: fn %s does not call record_dangerous (ADR 0013 R10)\n", fn_file, fn_line, fn_name; bad++ }
    in_fn = 0
}
BEGIN {
    n_ex = split(EXEMPT_LIST, ex, " ")
    bad = 0; checked = 0; direct = 0; delegated = 0
}
FNR == 1 { in_str = 0; in_fn = 0; skip = 0; pending_cfg = 0; pending_test = 0; depth = 0 }
{
    code = strip($0)
    if (skip) {
        depth += braces(code)
        if (depth <= 0) skip = 0
        next
    }
    if (in_fn) {
        fn_body = fn_body "\n" code
        depth += braces(code)
        if (semi_first) { in_fn = 0; next }
        if (opened && depth <= 0) finish()
        next
    }
    if (code ~ /^[ \t]*#\[cfg\((all\()?test[,)]/) { pending_cfg = 1; next }
    if (code ~ /^[ \t]*#\[([A-Za-z_]+::)?test[\](]/) { pending_test = 1; next }
    if (pending_cfg && code ~ /(^|[ \t])mod[ \t]+[A-Za-z0-9_]+[ \t]*\{/) {
        pending_cfg = 0; opened = 0; semi_first = 0; depth = 0
        depth = braces(code)
        if (depth > 0) skip = 1
        next
    }
    if (code ~ /^[ \t]*#\[/) next
    if (match(code, /(^|[^A-Za-z0-9_])fn[ \t]+[A-Za-z0-9_]+/)) {
        name = substr(code, RSTART, RLENGTH)
        sub(/^.*fn[ \t]+/, "", name)
        test_fn = pending_test || pending_cfg
        pending_cfg = 0; pending_test = 0
        if (name !~ /^(dangerous|insecure)_/ && name !~ /_(dangerous|insecure)$/) next
        if (test_fn) {
            opened = 0; semi_first = 0; depth = braces(code)
            if (depth > 0) skip = 1
            next
        }
        if (exempt(FILENAME, name)) next
        in_fn = 1; fn_name = name; fn_file = FILENAME; fn_line = FNR; fn_body = code
        opened = 0; semi_first = 0; depth = 0
        depth = braces(code)
        if (semi_first) { in_fn = 0; next }
        if (opened && depth <= 0) finish()
        next
    }
    if (code ~ /[^ \t]/) { pending_cfg = 0; pending_test = 0 }
}
END {
    printf "checked %d %d %d\n", checked, direct, delegated
    exit (bad > 0 ? 1 : 0)
}
AWK
)"

# Prints the problems and the `checked` line for the .rs files under the given directories.
check_dirs() {
    local files=()
    local f
    while IFS= read -r f; do
        files+=("$f")
        done < <(find "$@" -type f -name '*.rs' \
            -not -path '*/tests/*' -not -path '*/benches/*' -not -path '*/examples/*' \
        -not -path '*/target/*' -not -path '*/fuzz/*' | LC_ALL=C sort)
    if ((${#files[@]} == 0)); then
        echo "checked 0 0 0"
        return 0
    fi
    "${AWK:-awk}" -v EXEMPT_LIST="${EXEMPT[*]}" "$AWK_PROG" "${files[@]}"
}

self_test() {
    local tmp pass=0
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' RETURN
    mkdir -p "$tmp/crates/demo/src" "$tmp/crates/demo/tests"

    expect_pass() {
        local case="$1" out
        if ! out="$(check_dirs "$tmp/crates" 2>&1)"; then
            echo "FAIL $case: expected the fixture to pass, got:" >&2
            echo "$out" >&2
            return 1
        fi
        if [[ -n "${2:-}" ]] && ! grep -qF -- "$2" <<<"$out"; then
            echo "FAIL $case: no \"$2\" in:" >&2
            echo "$out" >&2
            return 1
        fi
        pass=$((pass + 1))
    }
    expect_fail() {
        local case="$1" want="$2" out
        if out="$(check_dirs "$tmp/crates" 2>&1)"; then
            echo "FAIL $case: expected the fixture to fail ($want), got:" >&2
            echo "$out" >&2
            return 1
        fi
        if ! grep -qF -- "$want" <<<"$out"; then
            echo "FAIL $case: failed without \"$want\":" >&2
            echo "$out" >&2
            return 1
        fi
        pass=$((pass + 1))
    }

    # ── every_dangerous_fn_records: direct calls, delegation, multi-line signatures ─────────
    cat >"$tmp/base.rs" <<'RS'
use id_effect::observability::posture::record_dangerous;

pub const ID: &str = "demo::Client::dangerous_skip";

pub struct Client;

impl Client {
    /// Records directly; the reason text holds braces: `{` and `}`.
    pub fn dangerous_skip(self) -> Self {
        record_dangerous(ID, "M0: skips the check {once} \
            and } again");
        self
    }

    /// Delegates to a checked opt-out.
    pub fn dangerous_skip_twice(self) -> Self {
        self.dangerous_skip()
    }

    pub fn insecure_plaintext<T: Send + 'static>(
        self,
        _t: T,
    ) -> Self {
        id_effect::observability::posture::record_dangerous(
            "demo::Client::insecure_plaintext",
            "plaintext",
        );
        self
    }

    pub fn layer_dangerous(self) -> Self {
        let brace = '{';
        let _ = brace;
        record_dangerous("demo::Client::layer_dangerous", "raw");
        self
    }

    /// Not an opt-out name.
    pub fn danger_zone(self) -> Self {
        self
    }
}

pub trait Policy {
    /// A declaration: checked where it is implemented.
    fn dangerous_cross(self) -> Self;
}

#[cfg(test)]
mod tests {
    #[test]
    fn dangerous_skip_is_recorded() {}

    fn dangerous_helper() {}
}

#[test]
fn dangerous_top_level_test() {}

#[cfg(test)]
pub fn dangerous_test_helper() {}

/// Prose naming fn dangerous_documented() is a comment, not a function.
pub fn documented() {}
RS
    cp "$tmp/base.rs" "$tmp/crates/demo/src/lib.rs"
    printf 'pub fn dangerous_in_tests_dir() {}\n' >"$tmp/crates/demo/tests/t.rs"
    expect_pass every_dangerous_fn_records "checked 4 3 1"

    # ── missing_record_fails ─────────────────────────────────────────────────────────────────
    cat >>"$tmp/crates/demo/src/lib.rs" <<'RS'

pub fn dangerous_forgot(x: u8) -> u8 {
    // record_dangerous("demo::dangerous_forgot", "a comment does not count");
    let _ = "record_dangerous(\"in a string\", \"does not count\")";
    x
}
RS
    expect_fail missing_record_fails "fn dangerous_forgot does not call record_dangerous"
    cat >"$tmp/crates/demo/src/more.rs" <<'RS'
impl Store {
    pub(crate) fn insecure_clear_all(&self) -> u64 {
        self.count
    }
}
RS
    expect_fail missing_record_fails "more.rs:2: fn insecure_clear_all does not call record_dangerous"
    # The suffix form is checked the same way.
    cp "$tmp/base.rs" "$tmp/crates/demo/src/lib.rs"
    printf 'pub fn clear_all_dangerous() {}\n' >"$tmp/crates/demo/src/more.rs"
    expect_fail missing_record_fails "more.rs:1: fn clear_all_dangerous does not call record_dangerous"

    # ── exempt_reader_passes ─────────────────────────────────────────────────────────────────
    rm "$tmp/crates/demo/src/more.rs"
    cp "$tmp/base.rs" "$tmp/crates/demo/src/lib.rs"
    mkdir -p "$tmp/crates/id_effect/src/observability"
    printf 'pub fn dangerous_opt_outs() -> Vec<u8> {\n    Vec::new()\n}\n' \
        >"$tmp/crates/id_effect/src/observability/posture.rs"
    printf 'pub fn record_dangerous(id: &str, reopens: &str) {\n    let _ = (id, reopens);\n}\n' \
        >>"$tmp/crates/id_effect/src/observability/posture.rs"
    expect_pass exempt_reader_passes "checked 4 3 1"

    echo "check-dangerous: self-test passed ($pass checks: every_dangerous_fn_records, missing_record_fails, exempt_reader_passes)"
}

if [[ "${1:-}" == "--self-test" ]]; then
    self_test
    exit 0
fi

dirs=("$@")
if ((${#dirs[@]} == 0)); then
    # Relative, so the problems read `crates/<crate>/src/…:line`.
    cd "$ROOT"
    dirs=(crates)
fi

status=0
out="$(check_dirs "${dirs[@]}")" || status=1
grep -v '^checked ' <<<"$out" || true
read -r _ checked direct delegated < <(grep '^checked ' <<<"$out")
if ((status != 0)); then
    echo "check-dangerous: opt-outs above do not record themselves (ADR 0013 R10)" >&2
    exit 1
fi
echo "check-dangerous: $checked dangerous_*/insecure_*/*_dangerous/*_insecure functions record their opt-out ($direct directly, $delegated by delegation)"
