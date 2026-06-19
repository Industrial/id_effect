#!/usr/bin/env bash
# Runs treefmt on the repo after Agent file edits. Treefmt is fast (Rust) and formats
# the whole repo; running it after each edit keeps everything consistent.
# Evidence: Cursor docs "Run formatters after edits" (https://cursor.com/docs/agent/hooks);
# project uses treefmt (package.json "format", treefmt.toml).

set -e
input=$(cat)
workspace_root=$(echo "$input" | jq -r '.workspace_roots[0] // ""')

if [[ -n "$workspace_root" && -d "$workspace_root" ]]; then
    (cd "$workspace_root" && devenv shell -- bun run format 2>/dev/null) || true
fi
exit 0
