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

ws_version="$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
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

# The per-manifest checker lives in check-version-drift.awk (prints one line per stale pin
# and a final "#checked N" line; exits 1 if any pin is stale).
AWK_CHECK="$ROOT/scripts/check-version-drift.awk"

drift=0
checked=0
for manifest in "${manifests[@]}"; do
    status=0
    out="$(awk -v want="$ws_version" -v file="$manifest" -f "$AWK_CHECK" "$manifest")" || status=$?
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
