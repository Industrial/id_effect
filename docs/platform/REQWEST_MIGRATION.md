# Reqwest → `id_effect_platform` HTTP migration

**Bead:** `iep-a-024` / `leaf-reqwest-migration`

## When to use which crate

| Need | Crate | Capability |
|------|-------|------------|
| Portable HTTP in `R` (`HttpClientKey`) | `id_effect_platform` | `execute`, `execute_stream` |
| Reqwest pools, JSON+Schema helpers | `id_effect_reqwest` | `ReqwestClientKey`, `send`, `json_schema` |
| Tokio services (both) | Register both providers at the composition root |

## Migration steps

1. Replace `Needs<ReqwestClientKey>` + `send(|c| c.get(url))` with `Needs<HttpClientKey>` + `execute(HttpRequest::get(url))`.
2. Register `provide_reqwest_http_client()` from `id_effect_platform::http` instead of (or alongside) `provide_reqwest_client`.
3. For streaming bodies use `execute_stream` → `Stream<u8, HttpError>` (chunked wire reads).
4. Keep `id_effect_reqwest` when you need `json_schema`, connection pools (`provide_reqwest_pool`), or direct `RequestBuilder` pipelines.

## Example provider graph

```rust
use id_effect::{build_env, provide};
use id_effect_platform::http::{provide_reqwest_http_client, TokioProcessRuntimeProvider};

let env = build_env([
  provide_reqwest_http_client(),
  provide!(TokioProcessRuntimeProvider),
])?;
```

## Non-goals

- Removing `id_effect_reqwest` (stays for reqwest-specific ergonomics).
- Auto-migrating every example in one pass — migrate incrementally per example crate.
