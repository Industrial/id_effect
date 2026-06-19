#!/usr/bin/env bash
# Blocks native Read/Grep; agents must use the lean-ctx MCP server instead.
# Evidence: lean-ctx INSTRUCTIONS.md, .cursor/rules/lean-ctx.mdc

set -euo pipefail
input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name // .toolName // empty')

case "$tool_name" in
    Read)
        cat <<'EOF'
{
  "permission": "deny",
  "user_message": "Native Read is disabled. Use the lean-ctx MCP server for file reads.",
  "agent_message": "Use CallMcpTool with server lean-ctx (or project-0-test-haskell-web-lean-ctx) and tool ctx_read. Modes: full (before edit), map (deps/API), signatures (API surface), diff (after edit), lines:N-M (range). Re-reads are cached (~13 tokens). Never retry native Read."
}
EOF
        exit 0
        ;;
    Grep)
        cat <<'EOF'
{
  "permission": "deny",
  "user_message": "Native Grep is disabled. Use the lean-ctx MCP server for code search.",
  "agent_message": "Use CallMcpTool with server lean-ctx and tool ctx_search. For directory listings use ctx_tree. For meaning-based search use ctx_semantic_search or roam-code roam_explore. Read the tool schema first. Never retry native Grep."
}
EOF
        exit 0
        ;;
esac

echo '{"permission":"allow"}'
exit 0
