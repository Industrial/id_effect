# id_effect_ai

Vendor-neutral LLM client traits for `id_effect` — buffered and streaming completions as `Effect` / `Stream`.

- `LanguageModel` capability trait
- `MockLanguageModel` for tests (no API keys)
- `mcp_server_template` example for MCP JSON-RPC over stdio

HTTP vendor adapters will use `id_effect_platform::HttpClient` in later leaves.
