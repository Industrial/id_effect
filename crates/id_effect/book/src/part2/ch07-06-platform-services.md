# Platform I/O (`id_effect_platform`)

Workspace crate **`id_effect_platform`** mirrors Effect.ts **`@effect/platform`**: **HTTP**, **filesystem**, and **process** as typed capabilities in [`Env`](../../src/capability/env.rs), not ad hoc `reqwest` / `std::fs` calls in domain code.

## Why separate from `id_effect`?

- **Ports, not drivers** — traits like `HttpClient`, `FileSystem`, `ProcessRuntime` describe *what* you need; provider impls install live or test doubles.
- **Test doubles** — `TestFileSystem` is in-memory; production uses `LiveFileSystemProvider`.
- **Stable HTTP boundary** — domain code depends on the `HttpClient` trait + `execute`, not `reqwest::RequestBuilder` (the capability key is crate-private).

## Modules

| Module | Responsibility |
|--------|----------------|
| `error` | `HttpError`, `FsError`, `ProcessError`, `PlatformError` |
| `http` | `HttpRequest` / `HttpResponse`, `HttpClient`, `ReqwestHttpClientProvider`, `execute` (key is internal) |
| `fs` | `FileSystem`, `LiveFileSystemProvider`, `TestFileSystem`, `read` |
| `process` | `CommandSpec`, `ProcessRuntime`, `spawn_wait` |
| `uri` | URI helpers |

## Wiring pattern

Each module declares a capability key and ships a default provider:

```rust
// in id_effect_platform::http (simplified; HttpClientKey is pub(crate))
#[derive(ProviderSpecDerive)]
#[provides(HttpClientKey)]
pub struct ReqwestHttpClientProvider;

impl ReqwestHttpClientProvider {
    fn new() -> Arc<dyn HttpClient> {
        Arc::new(ReqwestHttpClient::default_client())
    }
}
```

Application entry:

```rust
use id_effect::{run_with, RunError};
use id_effect_platform::http::{HttpRequest, execute, provide_reqwest_http_client};

let res = run_with(
    [provide_reqwest_http_client()],
    execute(HttpRequest::get("https://example.com")),
)
.map_err(|e| match e {
    RunError::Effect(e) => e,
    e => panic!("planner: {e}"),
})?;
```

Effects returned by `execute` carry the correct `Needs` bound internally; application code usually calls `run_with([provide_reqwest_http_client()], execute(req))` without naming the key.

Drive async platform effects with **`id_effect_tokio::run_async`** (see [Tokio bridge](./ch07-05-tokio-bridge.md)).

## Security note (filesystem)

`TestFileSystem` rejects paths containing `..`. Live I/O follows OS semantics — sandbox untrusted paths at a higher layer.

## Runnable example

```bash
cargo run -p id_effect_platform --example 010_platform_http_get
```

## Next

For reqwest-specific pools and JSON helpers, see [HTTP via reqwest](./ch07-07-reqwest-http.md).
