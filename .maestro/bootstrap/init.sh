#!/bin/bash
set -euo pipefail

echo "== Maestro Bootstrap Init =="

if [ -f package.json ]; then
    if command -v bun >/dev/null 2>&1; then
        echo "[ok] bun $(bun --version)"
        if [ ! -d "node_modules" ]; then
            echo "[...] Installing dependencies with bun"
            bun install
        else
            echo "[ok] node_modules already present"
        fi
    else
        echo "[!] package.json detected but bun is not installed"
        echo "    Install bun or customize .maestro/bootstrap/init.sh for this project"
    fi
fi

echo "[ok] Bootstrap init completed"
