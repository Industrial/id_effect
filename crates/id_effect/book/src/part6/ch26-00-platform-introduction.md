# Platform introduction

Part VI covers **application platform** capabilities: unified platform services, observability, data access, API boundaries, and the application host.

This portfolio extends the core `Effect` runtime and Part V functional patterns toward a full-stack Rust platform (compare .NET, Spring Boot, Rails, Django, Next.js capability trees).

## `id_effect_platform`

The [`id_effect_platform`](https://github.com/Industrial/id_effect/tree/main/crates/id_effect_platform) crate is the Rust analogue of `@effect/platform`. Each capability is a service in the typed environment `R` (Part II), provided by a `Layer`:

| Module | Trait | Capability / env | Layers |
|--------|-------|------------------|--------|
| `http` | `HttpClient` (`execute`, `execute_stream`) | `HttpClientService`, `HttpClientEnv` | `ReqwestHttpClientProvider::layer()`, `provide_http_client(client)` |
| `fs` | `FileSystem` | `FileSystemService`, `FileSystemEnv` | `LiveFileSystemProvider::layer()`, `provide_file_system(fs)` |
| `command` | `ProcessRuntime` (spawn, wait, kill OS children) | `ProcessRuntimeService`, `ProcessRuntimeEnv` | `TokioProcessRuntimeProvider::layer()`, `provide_process_runtime(rt)` |
| `uri` | — | — | — |
| `auth` (feature `auth`) | `SessionStore`, `OAuthClient` | — | `MemorySessionStore`; `MemoryOAuthClient` needs feature `test-util` |

The live implementations (reqwest, `tokio::process`) need a tokio runtime with its IO driver, so run them with `id_effect_tokio::TokioRuntime`:

```rust,no_run
use id_effect_platform::http::{HttpClientEnv, HttpRequest, ReqwestHttpClientProvider, execute};
use id_effect_tokio::TokioRuntime;

# async fn demo() {
let program = execute::<HttpClientEnv, _>(HttpRequest::get("https://example.com"))
    .provide_layer(ReqwestHttpClientProvider::layer());
let exit = TokioRuntime::current().expect("tokio runtime").run(program).await;
# }
```

In tests, provide a double directly: `execute(req).provide_all(http_client_env(canned)).block_on()`. Streaming responses expose an `HttpBody` read on demand (`next_chunk()`, `collect()`); build test bodies with `HttpBody::from_chunks` / `from_bytes`. Example: `cargo run -p id_effect_platform --example 010_platform_http_get`.

Prefer platform HTTP over a raw `reqwest::Client` in application `R` types. See [Reqwest migration](https://github.com/Industrial/id_effect/blob/main/docs/platform/REQWEST_MIGRATION.md).

## Portfolio map

| Chapter | Mission | Crate |
|---------|---------|-------|
| ch27 | Observability | `id_effect_opentelemetry` |
| ch28 | Data | `id_effect_sql` |
| ch29 | API boundaries | `id_effect_rpc` (labs, unpublished) |
| ch30 | Application host | `id_effect_axum` + `id_effect_platform::auth` (feature `auth`) |
| ch31, ch32, ch35 | Messaging, workflow, AI | `id_effect_jobs`, `id_effect_events`, `id_effect_workflow`, `id_effect_ai` (labs, unpublished) |

Mission index: [docs/platform/ROADMAP.md](https://github.com/Industrial/id_effect/blob/main/docs/platform/ROADMAP.md).

## Prerequisites

- Part II (capabilities and layers)
- Part V functional patterns (recommended)
