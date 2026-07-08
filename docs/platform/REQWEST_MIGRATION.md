# Reqwest HTTP — `id_effect_platform::http::reqwest`

**Bead:** `iep-a-024` / `leaf-reqwest-migration` — **completed** (logic lives in `id_effect_platform`).

## Where HTTP lives now

| Need | Import path |
|------|-------------|
| Portable HTTP in `R` (`HttpClientService`) | `id_effect_platform::http::{execute, execute_stream, provide_reqwest_http_client}` |
| Reqwest `RequestBuilder` pipelines, pools, JSON+Schema | `id_effect_platform::http::reqwest::{send, json_schema, provide_reqwest_client, …}` |

## Migration steps

1. Import reqwest helpers from `id_effect_platform::http::reqwest` and reqwest types from `reqwest` directly.
2. For new portable boundaries, prefer `HttpClientService` + `execute(HttpRequest::get(url))`.
3. Register providers at the composition root:

```rust
use id_effect::{build_env, provide};
use id_effect_platform::http::{provide_reqwest_http_client, HttpRequest, execute};
use id_effect_platform::http::reqwest::{provide_reqwest_client, send};
use id_effect_platform::process::TokioProcessRuntimeProvider;

let env = build_env([
  provide_reqwest_http_client(),
  provide!(TokioProcessRuntimeProvider),
])?;
```

## Examples

```bash
cargo run -p id_effect_platform --example 010_wiremock_get_text
cargo run -p id_effect_platform --example 020_wiremock_json
cargo run -p id_effect_platform --example 030_layer_default_env
```
