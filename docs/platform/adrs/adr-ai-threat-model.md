# ADR: `id_effect_ai` threat model

**Status:** Accepted  
**Date:** 2026-06-19  
**Context:** Phase H (`iep-h-011`) — security before vendor HTTP merges.

## Assets

- API keys (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `CURSOR_API_KEY`)
- User prompts and model responses (may contain PII)

## Threats and mitigations

| Threat | Mitigation |
|--------|------------|
| API key leakage via logs | `Secret<T>` — `Debug`/`Display` always `<redacted>` |
| API key in error messages | Map 401 to `AiError::Unauthorized` without echoing key |
| Prompt injection logged to telemetry | Spans record vendor + model + op only — not prompt text |
| CI live-key exfiltration | Wiremock/fixture tests in default CI; live tests `#[ignore]` |

## References

- [adr-ai-vendor-layout.md](./adr-ai-vendor-layout.md)
