#!/usr/bin/env bash
# Validates upstream Effect.ts parity tracking files (Phase I / platform-parity-hygiene).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CHECKLIST="${ROOT}/docs/effect-ts-parity/CHECKLIST-upstream-effect.md"
VERSION="${ROOT}/docs/effect-ts-parity/UPSTREAM-VERSION"

if [[ ! -f "${CHECKLIST}" ]]; then
    echo "missing checklist: ${CHECKLIST}" >&2
    exit 1
fi

if [[ ! -f "${VERSION}" ]]; then
    echo "missing version pin: ${VERSION}" >&2
    exit 1
fi

if ! grep -q 'Upstream area' "${CHECKLIST}"; then
    echo "checklist missing table header" >&2
    exit 1
fi

if ! grep -E '^effect@' "${VERSION}" >/dev/null; then
    echo "UPSTREAM-VERSION must contain an effect@ line" >&2
    exit 1
fi

if grep -E '^effect@TBD' "${VERSION}" >/dev/null; then
    echo "WARN: UPSTREAM-VERSION still effect@TBD — set after next upstream review" >&2
fi

echo "parity checklist files OK"
