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
3. Provide the client layers at the composition root. Since 0.7 providers are `Layer`s and effects
   run on a fiber runtime (the 0.6 `build_env([...])` / `provide!` registry is gone; see
   [MIGRATION-0.7.md](../MIGRATION-0.7.md)):

```rust
use id_effect_platform::http::{HttpRequest, execute, provide_reqwest_http_client};
use id_effect_platform::http::reqwest::{ReqwestClientLive, text};
use id_effect_tokio::TokioRuntime;

// Portable boundary: `HttpClientService` from a layer.
let portable = execute(HttpRequest::get(url)).provide_layer(provide_reqwest_http_client());
// Reqwest pipeline: a `reqwest::Client` from a layer (see example 030_layer_default_env).
let direct = text(move |c| c.get(other_url)).provide_layer(ReqwestClientLive::layer());

let rt = TokioRuntime::current().expect("tokio runtime");
let exit = rt.run(portable).await;
```

## Examples

```bash
cargo run -p id_effect_platform --example 010_wiremock_get_text
cargo run -p id_effect_platform --example 020_wiremock_json
cargo run -p id_effect_platform --example 030_layer_default_env
```
