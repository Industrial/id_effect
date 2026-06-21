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
    echo "::error::$pkg publish denied (403) — token lacks publish-new scope or crate allowlist entry"
    exit 1
fi
if echo "$output" | grep -qi 'readme .* does not appear to exist'; then
    echo "::error::$pkg missing readme declared in Cargo.toml"
    exit 1
fi
if echo "$output" | grep -qi 'no matching package named'; then
    echo "::error::$pkg publish blocked — internal dependency not on crates.io yet (check publish level order)"
    exit 1
fi
exit "$code"
