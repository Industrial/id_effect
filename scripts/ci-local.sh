#!/usr/bin/env bash
# Local CI parity with .github/workflows/ci.yml moon-ci "Moon run" step.
#
# Usage:
#   scripts/ci-local.sh              # auto: affected vs origin/main when on a branch
#   scripts/ci-local.sh affected     # PR-style affected + relations
#   scripts/ci-local.sh full         # full workspace (main push)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export MOON_TOOLCHAIN_FORCE_GLOBALS=rust
export MOON_CONCURRENCY="${MOON_CONCURRENCY:-1}"
mkdir -p "${TMPDIR:-$ROOT/tmp}"
export TMPDIR="${TMPDIR:-$ROOT/tmp}"

# CI uses dtolnay/stable; devenv defaults to nightly (Dylint). Prefer stable when wired.
if [ -n "${RUST_STABLE_BIN:-}" ] && [ -d "$RUST_STABLE_BIN" ]; then
    export PATH="$RUST_STABLE_BIN:$PATH"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target-ci-stable}"
    export CARGO_HOME="${CARGO_HOME:-$ROOT/.cargo-ci-stable}"
    mkdir -p "$CARGO_HOME"
fi

moon sync

MODE="${1:-auto}"
BASE="${CI_BASE:-origin/main}"
TASKS=( :ci-format :check :clippy :test :build :docs )

resolve_auto_mode() {
    if ! git rev-parse --verify "$BASE" >/dev/null 2>&1; then
        git fetch origin main 2>/dev/null || true
    fi
    if ! git rev-parse --verify "$BASE" >/dev/null 2>&1; then
        echo full
        return
    fi
    if git diff --quiet "$BASE"...HEAD 2>/dev/null; then
        echo full
    else
        echo affected
    fi
}

case "$MODE" in
    auto)
        MODE="$(resolve_auto_mode)"
        ;;
    affected | full) ;;
    *)
        echo "usage: $0 [auto|affected|full]" >&2
        exit 2
        ;;
esac

if [ "$MODE" = "affected" ]; then
    git fetch origin "${BASE#origin/}" 2>/dev/null || true
    moon run "${TASKS[@]}" \
        --affected \
        --include-relations \
        --base "$BASE" \
        --head HEAD
else
    moon run "${TASKS[@]}"
fi
