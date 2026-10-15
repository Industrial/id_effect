#!/usr/bin/env bash
# Run all example binaries for a given crate (or all workspace crates).
#
# Usage:
#   moon-run-crate-examples.sh <crate-name>    # single crate: cargo run -p <crate>
#   moon-run-crate-examples.sh --workspace     # all workspace crates with examples/
#
# Called by each crate's moon.yml `examples` task and by the CI workflow.
# Exits non-zero if any example fails; continues to run remaining examples
# so all failures are visible in one pass.
set -euo pipefail

FAILED=0

# Print the comma-separated `required-features` of example <name> in package <pkg> (empty if none).
example_required_features() {
    local PKG="$1"
    local CRATE_DIR="$2"
    local NAME="$3"
    cargo metadata --no-deps --format-version 1 --manifest-path "$CRATE_DIR/Cargo.toml" \
    | python3 -c "
import json, sys
pkg, name = sys.argv[1], sys.argv[2]
for p in json.load(sys.stdin).get('packages', []):
    if p['name'] != pkg:
        continue
    for t in p.get('targets', []):
        if t['name'] == name and 'example' in t.get('kind', []):
            print(','.join(t.get('required-features', [])))
" "$PKG" "$NAME"
}

run_examples_for_crate() {
    local PKG="$1"
    local CRATE_DIR="$2"

    if [ ! -d "$CRATE_DIR/examples" ]; then
        echo "  [skip] $PKG — no examples/ directory"
        return
    fi

    local FOUND=0
    for ex in "$CRATE_DIR"/examples/*.rs; do
        [ -f "$ex" ] || continue
        FOUND=1
        NAME="$(basename "$ex" .rs)"
        # Honour `[[example]] required-features` (e.g. id_effect_axum's tower examples):
        # without them `cargo run --example` refuses to build the target.
        local FEATURES
        FEATURES="$(example_required_features "$PKG" "$CRATE_DIR" "$NAME")"
        local FEATURE_ARGS=()
        if [ -n "$FEATURES" ]; then
            FEATURE_ARGS=(--features "$FEATURES")
        fi
        echo "  → cargo run -p $PKG --example $NAME ${FEATURE_ARGS[*]:-}"
        if ! cargo run -p "$PKG" --example "$NAME" "${FEATURE_ARGS[@]}"; then
            echo "  [FAIL] $PKG::$NAME"
            FAILED=1
        fi
    done

    if [ "$FOUND" -eq 0 ]; then
        echo "  [skip] $PKG — examples/ directory is empty"
    fi
}

if [ "${1:-}" = "--workspace" ]; then
    # Discover all workspace member crates from the root Cargo.toml members list.
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

    echo "Running examples for all workspace members…"
    # Use cargo metadata to get the authoritative list of workspace members.
    while IFS= read -r manifest_path; do
        CRATE_DIR="$(dirname "$manifest_path")"
        PKG="$(grep -m1 '^name' "$manifest_path" | sed 's/name = "\(.*\)"/\1/')"
        echo "[$PKG] ($CRATE_DIR)"
        run_examples_for_crate "$PKG" "$CRATE_DIR"
        done < <(
        cargo metadata --no-deps --format-version 1 \
            --manifest-path "$WORKSPACE_ROOT/Cargo.toml" \
        | python3 -c "
import json, sys
data = json.load(sys.stdin)
root = data.get('workspace_root', '')
for pkg in data.get('packages', []):
    print(pkg['manifest_path'])
"
    )
else
    # Single-crate mode: called by moon as  scripts/moon-run-crate-examples.sh <crate-name>
    PKG="${1:?Usage: $0 <crate-name> | --workspace}"
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

    # Resolve the directory for this crate by looking at Cargo.toml name fields.
    CRATE_DIR=""
    for d in "$WORKSPACE_ROOT"/crates/*/; do
        if grep -q "^name = \"$PKG\"" "$d/Cargo.toml" 2>/dev/null; then
            CRATE_DIR="$d"
            break
        fi
    done

    if [ -z "$CRATE_DIR" ]; then
        echo "Could not find crate '$PKG' under $WORKSPACE_ROOT/crates/"
        exit 1
    fi

    echo "[$PKG] Running examples…"
    run_examples_for_crate "$PKG" "$CRATE_DIR"
fi

if [ "$FAILED" -ne 0 ]; then
    echo ""
    echo "One or more examples failed."
    exit 1
fi

echo ""
echo "All examples passed."
