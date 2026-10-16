# Platform I/O (`id_effect_platform`)

Workspace crate **`id_effect_platform`** mirrors Effect.ts **`@effect/platform`**: **HTTP**,
**filesystem** and **process** as typed capabilities in `R`, not ad hoc `reqwest` / `std::fs`
calls in domain code.

## Why separate from `id_effect`?

- **Ports, not drivers** — traits like `HttpClient`, `FileSystem`, `ProcessRuntime` describe
  *what* you need; layers install live implementations or test doubles.
- **Test doubles** — `TestFileSystem` is in-memory; production uses `LiveFileSystemProvider`.
- **Stable HTTP boundary** — domain code depends on the `HttpClient` trait and `execute`, not on
  `reqwest::RequestBuilder`.

## Modules

| Module | Capability (`R`) | Layers | Effects |
|--------|------------------|--------|---------|
| `http` | `HttpClientService` (`HttpClientEnv`) | `ReqwestHttpClientProvider::layer()`, `provide_http_client(c)` | `execute`, `execute_stream` |
| `fs` | `FileSystemService` (`FileSystemEnv`) | `LiveFileSystemProvider::layer()`, `provide_file_system(fs)` | `read`, `exists` |
| `command` | `ProcessRuntimeService` (`ProcessRuntimeEnv`) | `TokioProcessRuntimeProvider::layer()`, `provide_process_runtime(rt)` | `spawn`, `spawn_wait`, `child_wait`, `child_kill` |
| `http::reqwest` | `ReqwestClient`, `ReqwestPool` | `ReqwestClientLive::layer()`, `provide_reqwest_client(c)`, `provide_reqwest_pool(n, ttl)` | `send`, `text`, `bytes`, `json`, `json_schema`, … |
| `auth` | — | — | `SessionStore`, `OAuthClient` and memory doubles |
| `error`, `uri` | — | — | `HttpError`, `FsError`, `ProcessError`; URI helpers |

Each capability is a newtype over `Arc<dyn Trait>` that derefs to the trait; each `*Env` alias is
the one-element list (`HttpClientEnv = HCons<HttpClientService, HNil>`), and `http_client_env(c)`
/ `file_system_env(fs)` / `process_runtime_env(rt)` build one for `.provide_all(..)`.

Every layer needs nothing (`In = HNil`) and is generic over the error type, so they merge into
one platform layer under your application's `E`:

```rust
# use id_effect::Layer;
# use id_effect_platform::command::TokioProcessRuntimeProvider;
# use id_effect_platform::fs::LiveFileSystemProvider;
# use id_effect_platform::http::ReqwestHttpClientProvider;
# struct AppError;
let platform: Layer<_, AppError> = LiveFileSystemProvider::layer()
    .merge(ReqwestHttpClientProvider::layer())
    .merge(TokioProcessRuntimeProvider::layer());
```

## Running an HTTP effect

`execute` is generic over any `R` that contains `HttpClientService`. When the effect is fed to
`provide_layer`, name `R` (from `examples/010_platform_http_get.rs`):

```rust
use id_effect::{Cause, Exit};
use id_effect_platform::http::{HttpClientEnv, HttpRequest, ReqwestHttpClientProvider, execute};
use id_effect_tokio::TokioRuntime;
# async fn get_example() -> Result<(), Box<dyn std::error::Error>> {

let program = execute::<HttpClientEnv, _>(HttpRequest::get("https://example.com"))
    .provide_layer(ReqwestHttpClientProvider::layer());
match TokioRuntime::current()?.run(program).await {
    Exit::Success(res) => println!("status={} bytes={}", res.status, res.body.len()),
    Exit::Failure(Cause::Fail(e)) => return Err(e.into()),
    Exit::Failure(c) => panic!("run failed: {c:?}"),
}
# Ok(())
# }
```

A test swaps in a canned client and runs anywhere, `block_on` included:

```rust
# use id_effect::{Effect, Exit, fail, succeed};
# use id_effect_platform::error::HttpError;
# use id_effect_platform::http::{
#     HttpClient, HttpRequest, HttpResponse, StreamingHttpResponse, execute, http_client_env,
# };
# struct Canned;
# impl HttpClient for Canned {
#     fn execute(&self, _req: HttpRequest) -> Effect<HttpResponse, HttpError> {
#         succeed(HttpResponse { status: 200, headers: vec![], body: b"ok".to_vec() })
#     }
#     fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
#         fail(HttpError::InvalidRequest("not canned".into()))
#     }
# }
# let url = "https://example.com";
let exit = execute(HttpRequest::get(url))
    .provide_all(http_client_env(Canned))
    .block_on();
# assert!(matches!(exit, Exit::Success(ref r) if r.status == 200));
```

The live implementations need a Tokio runtime **with its IO driver** (see
[Tokio bridge](./ch07-05-tokio-bridge.md)); the in-memory doubles (`TestFileSystem`,
`MemorySessionStore`, `MemoryOAuthClient`) do not.

Streaming responses (`execute_stream`) carry an `HttpBody` read on demand (`next_chunk()`,
`collect()`, both effects) with the `max_body_bytes` cap enforced as chunks arrive.

## Security note (filesystem)

`TestFileSystem` rejects paths containing `..`. Live I/O follows OS semantics — sandbox untrusted
paths at a higher layer.

## Runnable examples

```bash
cargo run -p id_effect_platform --example 010_platform_http_get
cargo run -p id_effect_platform --example 030_layer_default_env
```

## Next

For reqwest-specific pools and JSON helpers, see [HTTP via reqwest](./ch07-07-reqwest-http.md).
