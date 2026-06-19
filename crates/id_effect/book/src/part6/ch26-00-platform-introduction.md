# Platform introduction

Part VI covers **application platform** capabilities: unified platform services, observability, data access, API boundaries, and the application host.

This portfolio extends the core `Effect` runtime and Part V functional patterns toward a full-stack Rust platform (compare .NET, Spring Boot, Rails, Django, Next.js capability trees).

## `id_effect_platform`

The [`id_effect_platform`](https://github.com/Industrial/id_effect/tree/main/crates/id_effect_platform) crate is the Rust analogue of `@effect/platform`:

| Module | Trait | Purpose |
|--------|-------|---------|
| `http` | `HttpClient` | Buffered and streaming HTTP (`execute`, `execute_stream`) |
| `fs` | `FileSystem` | Read/write/append/exists/metadata |
| `process` | `ProcessRuntime` | Spawn, wait, kill child processes |
| `uri` | — | Portable URI parsing |
| `auth` | `SessionStore`, `OAuthClient` | Session and OAuth capability traits |

Capabilities are registered with `provide_reqwest_http_client()`, `LiveFileSystemProvider`, and `TokioProcessRuntimeProvider`.

```rust
use id_effect::{build_env, run_async};
use id_effect_platform::http::{HttpRequest, execute, provide_reqwest_http_client};

let env = build_env([provide_reqwest_http_client()]).expect("providers");
let resp = run_async(execute(HttpRequest::get("https://example.com")), env).await?;
```

Prefer platform HTTP over raw `reqwest::Client` in application `R` types. See [Reqwest migration](../../../docs/platform/REQWEST_MIGRATION.md).

## Portfolio map

| Chapter | Mission | Crate |
|---------|---------|-------|
| ch27 | Observability | `id_effect_opentelemetry` |
| ch28 | Data | `id_effect_sql` |
| ch29 | API boundaries | `id_effect_rpc` |
| ch30 | Application host | `id_effect_axum` + `id_effect_platform::auth` |

Mission index: [docs/platform/ROADMAP.md](../../../docs/platform/ROADMAP.md).

## Prerequisites

- Part II (capabilities and layers)
- Part V functional patterns (recommended)
