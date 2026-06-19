# `id_effect_platform`

Cross-cutting **platform** traits (HTTP, filesystem, process) for [`id_effect`](../id_effect), aligned with Effect.ts [`@effect/platform`](https://effect.website/docs/platform/introduction).

## Modules

| Module | Description |
|--------|-------------|
| [`error`](src/error.rs) | `HttpError`, `FsError`, `ProcessError`, `PlatformError` |
| [`http`](src/http.rs) | `HttpClient`, `ReqwestHttpClient`, `HttpClientKey`, portable request/response |
| [`fs`](src/fs.rs) | `FileSystem`, `LiveFileSystem`, `TestFileSystem`, `FileSystemKey` |
| [`process`](src/process.rs) | `ProcessRuntime`, `TokioProcessRuntime`, `ProcessRuntimeKey` |
| [`uri`](src/uri.rs) | `http::Uri` parse / build helpers |
| [`auth`](src/auth/mod.rs) | `SessionStore`, `OAuthClient` capability traits |

## Design

See RFC [0001-id-effect-platform.md](../../docs/effect-ts-parity/rfcs/0001-id-effect-platform.md).

## Testing

Unit and integration tests follow the repository root **[`TESTING.md`](../../TESTING.md)** (BDD-style names, nested `#[cfg(test)]` modules beside implementation, `rstest` where inputs form a table). Integration tests under `tests/` complement wire/process scenarios; run `cargo test -p id_effect_platform`.

## HTTP modules

| Module | Role |
|--------|------|
| [`http`](src/http/mod.rs) | Portable `HttpClient`, `execute`, `execute_stream` |
| [`http::reqwest`](src/http/reqwest.rs) | `send`, `json`, `json_schema`, pools (`provide_reqwest_pool`) |

## Runnable examples

```bash
cargo run -p id_effect_platform --example 010_platform_http_get
cargo run -p id_effect_platform --example 010_wiremock_get_text
```
