# `id_effect_platform`

Cross-cutting **platform** capabilities (HTTP, filesystem, subprocess commands) for [`id_effect`](../id_effect), aligned with Effect.ts [`@effect/platform`](https://effect.website/docs/platform/introduction).

Each boundary is a capability (`HttpClientService`, `FileSystemService`, `ProcessRuntimeService`) in the typed environment of an `id_effect::next::Effect`, provided by an `id_effect::next::Layer`: production code provides the live layer (reqwest, `tokio::fs`, `tokio::process`), tests provide a double — the effect code does not change.

## Quick start

```rust
use id_effect::Exit;
use id_effect::next::{Effect, fail, succeed};
use id_effect_platform::error::HttpError;
use id_effect_platform::http::{
  HttpClient, HttpClientEnv, HttpRequest, HttpResponse, StreamingHttpResponse, execute,
  provide_http_client,
};

/// Test double: echoes the URL back as the body.
struct Canned;

impl HttpClient for Canned {
  fn execute(&self, req: HttpRequest) -> Effect<HttpResponse, HttpError> {
    succeed(HttpResponse { status: 200, headers: vec![], body: req.url.into_bytes() })
  }

  fn execute_stream(&self, _req: HttpRequest) -> Effect<StreamingHttpResponse, HttpError> {
    fail(HttpError::InvalidRequest("no streaming in this double".into()))
  }
}

fn main() {
  // Production: `.provide_layer(ReqwestHttpClientProvider::layer())`, run on
  // `id_effect_tokio::TokioRuntime` (reqwest needs the tokio IO driver).
  let program = execute::<HttpClientEnv, _>(HttpRequest::get("https://example.com/hi"))
    .provide_layer(provide_http_client(Canned));
  let Exit::Success(res) = program.block_on() else { panic!("request failed") };
  assert_eq!(res.status, 200);
  assert_eq!(res.body, b"https://example.com/hi");
}
```

The layers merge into one platform environment:

```rust,ignore
let platform = LiveFileSystemProvider::layer()
  .merge(ReqwestHttpClientProvider::layer())
  .merge(TokioProcessRuntimeProvider::layer());
let exit = TokioRuntime::current()?.run(program.provide_layer(platform)).await;
```

## Modules

| Module | Description |
|--------|-------------|
| [`error`](src/error.rs) | `HttpError`, `FsError`, `ProcessError`, `PlatformError` |
| [`http`](src/http/mod.rs) | `HttpClient`, `ReqwestHttpClient`, `HttpBody`, `execute`, `execute_stream`; layers `ReqwestHttpClientProvider::layer` / `provide_reqwest_http_client`, `provide_http_client` |
| [`http::reqwest`](src/http/reqwest.rs) | `send`, `text`, `json`, `json_schema`, `send_pooled`; layers `ReqwestClientLive::layer`, `provide_reqwest_client`, `provide_reqwest_pool` |
| [`fs`](src/fs.rs) | `FileSystem`, `LiveFileSystem`, `TestFileSystem`, `read`, `exists`; layers `LiveFileSystemProvider::layer`, `provide_file_system` |
| [`command`](src/command.rs) (was `process`) | `ProcessRuntime`, `TokioProcessRuntime`, `spawn`, `spawn_wait`, `child_wait`, `child_kill`; layers `TokioProcessRuntimeProvider::layer`, `provide_process_runtime` |
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
cargo run -p id_effect_platform --example 152_cookbook_bounded_command_pool
```
