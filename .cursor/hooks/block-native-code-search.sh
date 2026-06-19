#!/usr/bin/env bash
# Blocks native SemanticSearch; agents must use roam-code (or lean-ctx) MCP instead.
# Evidence: roam-code INSTRUCTIONS.md, lean-ctx INSTRUCTIONS.md

set -euo pipefail
input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name // .toolName // empty')

case "$tool_name" in
    SemanticSearch)
        cat <<'EOF'
{
  "permission": "deny",
  "user_message": "Native SemanticSearch is disabled. Use roam-code or lean-ctx MCP for code exploration.",
  "agent_message": "Prefer CallMcpTool with server roam-code (or project-0-test-haskell-web-roam-code): roam_explore (broad exploration), roam_understand (deep dive), roam_search_symbol (symbol lookup), roam_context (file/module context), roam_trace (call paths), roam_uses (references). For token-compressed semantic search use lean-ctx ctx_semantic_search. Read tool schemas first. Never retry native SemanticSearch."
}
EOF
        exit 0
        ;;
esac

echo '{"permission":"allow"}'
exit 0
