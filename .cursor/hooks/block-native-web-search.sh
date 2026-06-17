#!/usr/bin/env bash
# Blocks native WebSearch/WebFetch; agents must use SearXNG or Context7 MCP instead.
# Evidence: .cursor/rules/agent-tool-routing.mdc, .cursor/rules/mcp-servers.mdc

set -euo pipefail
input=$(cat)
tool_name=$(echo "$input" | jq -r '.tool_name // .toolName // empty')
tool_input=$(echo "$input" | jq -r '.tool_input // .arguments // empty')

looks_like_library_docs() {
    local url="${1:-}"
    [[ -z "$url" ]] && return 1
    echo "$url" | grep -qiE \
        'docs\.|documentation|readthedocs|github\.com/.+/wiki|/api/reference|developer\.|/guide/|/reference/|npmjs\.com/package|hexdocs\.pm|hex\.pm/docs|pkg\.go\.dev|docs\.rs|doc\.rust-lang|pub\.dev/documentation|kotlinlang\.org/docs|learn\.microsoft\.com|developer\.mozilla\.org|swagger|openapi'
}

case "$tool_name" in
    WebSearch)
        cat <<'EOF'
{
  "permission": "deny",
  "user_message": "Native WebSearch is disabled. Use the SearXNG MCP server (http://localhost:4001).",
  "agent_message": "Use CallMcpTool with server searxng (or project-0-test-haskell-web-searxng) and tool searxng_web_search. For library/framework API docs use Context7 instead: resolve-library-id then query-docs. Read tool schemas first. Never retry native WebSearch."
}
EOF
        exit 0
        ;;
    WebFetch)
        url=$(echo "$tool_input" | jq -r '.url // empty' 2>/dev/null || true)
        if looks_like_library_docs "$url"; then
            cat <<'EOF'
{
  "permission": "deny",
  "user_message": "Native WebFetch is disabled for library docs. Use the Context7 MCP server.",
  "agent_message": "This URL looks like library documentation. Use CallMcpTool with server context7: (1) resolve-library-id with libraryName and query, unless the user gave an explicit /org/project ID; (2) query-docs with libraryId and query. For general web pages use searxng web_url_read instead. Never retry native WebFetch."
}
EOF
        else
            cat <<'EOF'
{
  "permission": "deny",
  "user_message": "Native WebFetch is disabled. Use SearXNG web_url_read for page content.",
  "agent_message": "Use CallMcpTool with server searxng and tool web_url_read for general URLs. For library/framework API documentation use Context7: resolve-library-id then query-docs (skip resolve when user provides /org/project ID). Read tool schemas first. Never retry native WebFetch."
}
EOF
        fi
        exit 0
        ;;
esac

echo '{"permission":"allow"}'
exit 0
