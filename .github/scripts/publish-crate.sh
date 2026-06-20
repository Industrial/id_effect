#!/usr/bin/env bash
set -euo pipefail

pkg=$1
shift
args=(--package "$pkg" --no-verify "$@")
if [ "${DRY_RUN:-false}" = "true" ]; then
    args+=(--dry-run)
fi

set +e
output=$(cargo publish "${args[@]}" 2>&1)
code=$?
set -e
echo "$output"
if [ "$code" -eq 0 ]; then
    exit 0
fi
if echo "$output" | grep -qiE 'already (uploaded|exists)'; then
    echo "$pkg already on crates.io — skipping"
    exit 0
fi
if echo "$output" | grep -qi 'required permissions'; then
    echo "::warning::$pkg publish denied (403) — token may lack publish-new scope or crate allowlist entry"
    exit 0
fi
if echo "$output" | grep -qi 'no matching package named'; then
    echo "::warning::$pkg publish skipped — registry dependency not published yet"
    exit 0
fi
exit "$code"
