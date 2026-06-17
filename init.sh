#!/usr/bin/env bash
# Project init -- emitted once by `maestro setup` and never overwritten.
# Edit freely; Maestro will not touch this file again unless you delete it.
set -euo pipefail

# Check maestro is available
if ! command -v maestro &> /dev/null; then
    echo "maestro not found in PATH." >&2
    echo "Install maestro or ensure it's in your PATH." >&2
    echo "If installed to a custom location, add it to PATH or set MAESTRO_BIN." >&2
    exit 1
fi

# Health gate -- exits non-zero if .maestro/ scaffold is broken.
maestro doctor

# Cold-start view -- one-screen resume snapshot.
maestro status
