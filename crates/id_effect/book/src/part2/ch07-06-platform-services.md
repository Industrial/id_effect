# Platform I/O (`id_effect_platform`)

Workspace crate **`id_effect_platform`** mirrors Effect.ts **`@effect/platform`**: **HTTP**, **filesystem**, and **process** as typed capabilities in [`Env`](../../src/capability/env.rs), not ad hoc `reqwest` / `std::fs` calls in domain code.

## Why separate from `id_effect`?

- **Ports, not drivers** — traits like `HttpClient`, `FileSystem`, `ProcessRuntime` describe *what* you need; [`ProviderSpec`](../../src/capability/provider.rs) impls install live or test doubles.
- **Test doubles** — `TestFileSystem` is in-memory; production uses `LiveFileSystemProvider`.
- **Stable HTTP boundary** — domain code depends on `HttpClientKey` + `execute`, not `reqwest::RequestBuilder`.

## Modules

| Module | Responsibility |
|--------|----------------|
| `error` | `HttpError`, `FsError`, `ProcessError`, `PlatformError` |
| `http` | `HttpRequest` / `HttpResponse`, `HttpClient`, `ReqwestHttpClientProvider`, `execute` |
| `fs` | `FileSystem`, `LiveFileSystemProvider`, `TestFileSystem`, `read` |
| `process` | `CommandSpec`, `ProcessRuntime`, `spawn_wait` |
| `uri` | URI helpers |

## Wiring pattern (v2)

Each module declares a key via `define_capability!` and ships a default provider:

```rust
// in id_effect_platform::http (simplified)
define_capability!(HttpClientKey, Arc<dyn HttpClient>);

pub struct ReqwestHttpClientProvider;

impl ProviderSpec for ReqwestHttpClientProvider {
    type Key = HttpClientKey;
    type Output = Arc<dyn HttpClient>;
    fn provider_id() -> &'static str { "platform/http/reqwest-default" }
    fn provide(_deps: &Env) -> Result<Arc<dyn HttpClient>, ProviderError> {
        Ok(Arc::new(ReqwestHttpClient::default_client()))
    }
}
```

Application entry:

```rust
use id_effect::{provide, run_with, RunError};
use id_effect_platform::http::{HttpRequest, ReqwestHttpClientProvider, execute};

let res = run_with(
    [provide!(ReqwestHttpClientProvider)],
    execute(HttpRequest::get("https://example.com")),
)
.map_err(|e| match e {
    RunError::Effect(e) => e,
    e => panic!("planner: {e}"),
})?;
```

Effects that call `execute` require `R: Needs<HttpClientKey>`. Use [`require!`](../../src/capability/run.rs) inside handlers or rely on helper functions that already bound `Needs`.

Drive async platform effects with **`id_effect_tokio::run_async`** (see [Tokio bridge](./ch07-05-tokio-bridge.md)).

## Security note (filesystem)

`TestFileSystem` rejects paths containing `..`. Live I/O follows OS semantics — sandbox untrusted paths at a higher layer.

## Runnable example

```bash
cargo run -p id_effect_platform --example 010_platform_http_get
```

## Next

For reqwest-specific pools and JSON helpers, see [HTTP via reqwest](./ch07-07-reqwest-http.md).
