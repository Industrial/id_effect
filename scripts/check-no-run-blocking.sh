#!/usr/bin/env bash
# G-nb: no thread-parking run edges in library code.
#
# Lints the lib target of a workspace crate with the disallowed-methods list in
# scripts/clippy-no-blocking/clippy.toml (v1 `run_blocking`, `next` `block_on` /
# `try_block_on` on `Effect` and `Runtime`). Tests, benches and examples are not checked.
# An edge call site is allowlisted in the source with
# `#[allow(clippy::disallowed_methods, reason = "edge: ...")]`.
#
# Usage:
#   scripts/check-no-run-blocking.sh P             # fail (exit 1) on any hit
#   scripts/check-no-run-blocking.sh --report P    # list hits and a count, always exit 0
#
# P is a workspace package name (`id_effect_cli`) or a directory / file inside one
# (`crates/id_effect/src/next/coordination`): a path lints its owning crate and keeps only the
# hits under that path.
#
# Lints run in a separate target dir (default `<target>/clippy-no-blocking`, override with
# NB_TARGET_DIR) so the alternate clippy config does not invalidate the normal clippy cache.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

usage() {
    echo "usage: $0 [--report] <package-or-path>" >&2
    exit 2
}

REPORT=0
TARGET=""
while [ $# -gt 0 ]; do
    case "$1" in
        --report) REPORT=1 ;;
        -h | --help) usage ;;
        -*) usage ;;
        *)
            [ -z "$TARGET" ] || usage
            TARGET="$1"
            ;;
    esac
    shift
done
[ -n "$TARGET" ] || usage

META="$(cargo metadata --no-deps --format-version 1)"

# Resolve TARGET to (package, path filter relative to the workspace root).
FILTER=""
if [ -e "$TARGET" ]; then
    ABS="$(realpath "$TARGET")"
    PKG="$(jq -r --arg p "$ABS" '
        [ .packages[]
          | { name, dir: (.manifest_path | sub("/Cargo.toml$"; "")) }
          | select(.dir as $d | ($p + "/") | startswith($d + "/")) ]
        | sort_by(.dir | length) | last | .name // empty' <<<"$META")"
    [ -n "$PKG" ] || {
        echo "check-no-run-blocking: $TARGET is not inside a workspace package" >&2
        exit 2
    }
    FILTER="$(realpath --relative-to="$ROOT" "$ABS")"
else
    PKG="$TARGET"
    jq -e --arg n "$PKG" 'any(.packages[]; .name == $n)' <<<"$META" >/dev/null || {
        echo "check-no-run-blocking: no workspace package or path named '$PKG'" >&2
        exit 2
    }
fi

if ! jq -e --arg n "$PKG" \
    'any(.packages[] | select(.name == $n) | .targets[]; any(.kind[]; . == "lib" or . == "proc-macro"))' \
    <<<"$META" >/dev/null; then
    echo "check-no-run-blocking: $PKG has no lib target; nothing to check"
    exit 0
fi

PKG_ID="$(jq -r --arg n "$PKG" '.packages[] | select(.name == $n) | .id' <<<"$META")"
BASE_TARGET="$(jq -r '.target_directory' <<<"$META")"
export CARGO_TARGET_DIR="${NB_TARGET_DIR:-$BASE_TARGET/clippy-no-blocking}"
export CLIPPY_CONF_DIR="$ROOT/scripts/clippy-no-blocking"

OUT="$(mktemp)"
ERR="$(mktemp)"
trap 'rm -f "$OUT" "$ERR"' EXIT

# The cargo exit status is checked below through the diagnostics, not here: a crate that
# `deny`s warnings turns the hits themselves into errors.
set +e
cargo clippy -p "$PKG" --lib --all-features --message-format=json \
    -- -W clippy::disallowed_methods >"$OUT" 2>"$ERR"
CARGO_STATUS=$?
set -e

# A config entry that no longer resolves silently disables that check: fail loudly instead.
STALE="$(jq -r '
    select(.reason == "compiler-message")
    | .message.message
    | select(test("does not refer to (a reachable|an existing)"))' "$OUT" | sort -u)"
STALE="$STALE$(grep -E 'does not refer to (a reachable|an existing)' "$ERR" | sed 's/\x1b\[[0-9;]*m//g' | sort -u || true)"
if [ -n "$STALE" ]; then
    printf '%s\n' "$STALE" >&2
    echo "check-no-run-blocking: stale path in scripts/clippy-no-blocking/clippy.toml" >&2
    exit 2
fi

# Hits: `file:line:col: message`, in PKG's lib only (`cargo clippy -p` also lints path deps),
# restricted to FILTER when one is given.
HITS="$(jq -r --arg f "$FILTER" --arg id "$PKG_ID" '
    select(.reason == "compiler-message" and .package_id == $id)
    | .message
    | select(.code.code? == "clippy::disallowed_methods")
    | (.spans[] | select(.is_primary)) as $s
    | select($f == "" or ($s.file_name | startswith($f)))
    | "\($s.file_name):\($s.line_start):\($s.column_start): \(.message)"' "$OUT" | sort -u)"

# Any other error means the crate did not build: the result would be incomplete.
OTHER_ERRORS="$(jq -r '
    select(.reason == "compiler-message")
    | .message
    | select(.level == "error" and (.code.code? // "") != "clippy::disallowed_methods")
    | select(.message | startswith("aborting due to") | not)
    | .rendered' "$OUT")"
if [ "$CARGO_STATUS" -ne 0 ] && { [ -n "$OTHER_ERRORS" ] || [ -z "$HITS" ]; }; then
    if [ -n "$OTHER_ERRORS" ]; then printf '%s\n' "$OTHER_ERRORS" >&2; else tail -n 20 "$ERR" >&2; fi
    echo "check-no-run-blocking: $PKG failed to build; result incomplete" >&2
    exit 2
fi

SCOPE="$PKG${FILTER:+ ($FILTER)}"
if [ -z "$HITS" ]; then
    echo "check-no-run-blocking: $SCOPE clean (0 hits)"
    exit 0
fi

COUNT="$(printf '%s\n' "$HITS" | wc -l | tr -d ' ')"
printf '%s\n' "$HITS"
echo "check-no-run-blocking: $SCOPE: $COUNT blocking call site(s) in lib code"
if [ "$REPORT" -eq 1 ]; then
    exit 0
fi
exit 1
