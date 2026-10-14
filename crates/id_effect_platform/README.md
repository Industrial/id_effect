# `id_effect_platform`

Cross-cutting **platform** capabilities (HTTP, filesystem, subprocess commands) for [`id_effect`](../id_effect), aligned with Effect.ts [`@effect/platform`](https://effect.website/docs/platform/introduction).

Each boundary is a capability trait stored in an `id_effect::Env`: production code registers the live implementation (reqwest, `tokio::fs`, `tokio::process`), tests swap in a double — the effect code does not change.

## Quick start

```rust
use std::sync::Arc;

use id_effect::{Effect, Env, fail, run_blocking, succeed};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClient, HttpRequest, HttpResponse, StreamingHttpResponse, env_set_http_client, execute,
};

/// Test double: echoes the URL back as the body.
struct Canned;

impl HttpClient for Canned {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError, ()> {
    succeed(HttpResponse { status: 200, headers: vec![], body: req.url.into_bytes() })
  }

  fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError, ()> {
    fail(HttpError::InvalidRequest("no streaming in this double".into()))
  }
}

fn main() {
  // Production: `id_effect::run_with([provide_reqwest_http_client()], execute(..))`.
  let mut env = Env::new();
  env_set_http_client(&mut env, Arc::new(Canned));

  let res = run_blocking(execute::<Env>(HttpRequest::get("https://example.com/hi")), env).unwrap();
  assert_eq!(res.status, 200);
  assert_eq!(res.body, b"https://example.com/hi");
}
```

## Modules

| Module | Description |
|--------|-------------|
| [`error`](src/error.rs) | `HttpError`, `FsError`, `ProcessError`, `PlatformError` |
| [`http`](src/http/mod.rs) | `HttpClient`, `ReqwestHttpClient`, `execute`, `execute_stream`, `provide_reqwest_http_client` |
| [`http::reqwest`](src/http/reqwest.rs) | `send`, `json`, `json_schema`, pools (`provide_reqwest_pool`) |
| [`fs`](src/fs.rs) | `FileSystem`, `LiveFileSystem`, `TestFileSystem`, `FileSystemKey` |
| [`command`](src/command.rs) (was `process`) | `ProcessRuntime`, `TokioProcessRuntime`, `ProcessRuntimeKey` |
| [`uri`](src/uri.rs) | `http::Uri` parse / build helpers |

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `auth` | off | `SessionStore`, `OAuthClient` capability traits ([`auth`](src/auth/mod.rs)). Web-framework material, opt-in since 0.6. |
| `test-util` | off | In-memory test doubles (`MemoryOAuthClient`); implies `auth`. |

The HTTP client uses **reqwest 0.13** (rustls, gzip, json).

## Design

See RFC [0001-id-effect-platform.md](../../docs/effect-ts-parity/rfcs/0001-id-effect-platform.md).

## Testing

Unit and integration tests follow the repository root **[`TESTING.md`](../../TESTING.md)** (BDD-style names, nested `#[cfg(test)]` modules beside implementation, `rstest` where inputs form a table). Integration tests under `tests/` complement wire/process scenarios; run `cargo test -p id_effect_platform`.

## Runnable examples

```bash
cargo run -p id_effect_platform --example 010_platform_http_get
cargo run -p id_effect_platform --example 010_wiremock_get_text
cargo run -p id_effect_platform --example 020_wiremock_json
cargo run -p id_effect_platform --example 030_layer_default_env
```
