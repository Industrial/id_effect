#!/usr/bin/env bash
# Pre-commit: auto-format, then CI-parity checks on affected crates (stable clippy).
# Pre-push runs scripts/ci-local.sh plus coverage and audit.
unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE

set -euo pipefail
mkdir -p tmp
exec env TMPDIR="$(pwd)/tmp" MOON_CONCURRENCY=1 devenv shell -- bash -lc '
  set -euo pipefail
  moon run :format
  bash scripts/ci-local.sh affected
'
