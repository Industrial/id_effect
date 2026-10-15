# `id_effect_platform`: effects on the 0.7 kernel, providers are Layers

0.7 moves `id_effect_platform` to the new kernel (`id_effect::next`). Every trait method and
free function returns a `next::Effect`, and the capabilities live in the typed `next`
environment instead of a v1 `Env`. The v1 `ProviderBox` / `ProviderSpecDerive` providers are
replaced by `next::Layer`s.

## Capabilities and layers

| capability | 0.6 | 0.7 capability | 0.7 layers |
|---|---|---|---|
| HTTP | `HttpClientService = Arc<dyn HttpClient>`, `provide_reqwest_http_client() -> ProviderBox`, `env_set_http_client`, `env_has_http_client` | `HttpClientService(pub Arc<dyn HttpClient>)` (derefs to `dyn HttpClient`), `HttpClientEnv` | `ReqwestHttpClientProvider::layer()` (also `provide_reqwest_http_client()`), `provide_http_client(client)`; `http_client_env(client)` for `.provide_all` |
| filesystem | `FileSystemService = Arc<dyn FileSystem>`, `provide!(LiveFileSystemProvider)` | `FileSystemService(pub Arc<dyn FileSystem>)`, `FileSystemEnv` | `LiveFileSystemProvider::layer()`, `provide_file_system(fs)`; `file_system_env(fs)` |
| command | `ProcessRuntimeService = Arc<dyn ProcessRuntime>`, `provide!(TokioProcessRuntimeProvider)` | `ProcessRuntimeService(pub Arc<dyn ProcessRuntime>)`, `ProcessRuntimeEnv` | `TokioProcessRuntimeProvider::layer()`, `provide_process_runtime(rt)`; `process_runtime_env(rt)` |
| reqwest client | `ReqwestClient`, `provide!(ReqwestClientLive)`, `provide_reqwest_client(c) -> ProviderBox` | `ReqwestClient` (unchanged), `ReqwestClientEnv` | `ReqwestClientLive::layer()`, `provide_reqwest_client(c)` |
| reqwest pool | `ReqwestPool = v1 Pool<PooledClient, Never>`, `provide_reqwest_pool(n, ttl) -> ProviderBox` | `ReqwestPool = next::Pool<PooledClient>`, `ReqwestPoolEnv` | `provide_reqwest_pool(n, ttl)` (a new pool per build) |

Every layer is `Layer<Env, E>` with `In = HNil` and a free error type `E`, so the three merge into
one platform layer under the application's error type:

```rust
let platform: Layer<_, AppError> = LiveFileSystemProvider::layer()
  .merge(ReqwestHttpClientProvider::layer())
  .merge(TokioProcessRuntimeProvider::layer());
```

## Functions

- `fs::{read, exists}`, `command::{spawn, spawn_wait}`, `http::{execute, execute_stream}` and the
  `http::reqwest` helpers (`send`, `text`, `bytes`, `json`, `json_schema`, `send_pooled`, `get`,
  `post`, `put`, `delete`, `head`, `patch`) take `R: Selector<Service, I> + HList` instead of
  `R: Needs<Service>`. The index `I` is a new generic parameter (inferred; one more `_` in a
  turbofish: `text::<String, Error, _, _>(..)` still has four, because `build` is now an
  `impl FnOnce` argument).
- When the effect is fed to `provide_layer`, name `R`: `execute::<HttpClientEnv, _>(req)`.
  `provide_all(http_client_env(..))` infers it.
- `child_wait::<R>` / `child_kill::<R>` take any `R: Send + Sync + 'static`.
- `ProcessRuntime::spawn_wait` is now `spawn(..).flat_map(child_wait)`.

## Streaming bodies

`StreamingHttpResponse::body` was a v1 `Stream<u8, HttpError, ()>` fed by a spawned task; it is now
an `HttpBody`, read on demand (`next_chunk()`, `collect()`, both `next` effects), with the
`max_body_bytes` cap enforced as chunks arrive. Test doubles build one with `HttpBody::from_chunks`,
`HttpBody::from_bytes(bytes, chunk_size)` or `HttpBody::empty()`.

- `body_as_chunk_stream(body, n)` → `HttpBody::from_bytes(&body, n)`.
- `response_body_stream(&resp, n)` now returns an `HttpBody`.
- `response_body_chunk(&resp)` (a v1 `Chunk<u8>`) is removed: use `resp.body`.

## Runtime

The live implementations need a tokio runtime **with its IO driver** (reqwest, `tokio::process`):
run them with `id_effect_tokio::TokioRuntime` (`TokioRuntime::current()?.run(eff).await`), or with
`next::Runtime::from_executor(TokioExecutor::current())` inside such a runtime. The in-memory
doubles (`TestFileSystem`, `MemorySessionStore`, `MemoryOAuthClient`) run anywhere, `block_on`
included.

## Before / after

```rust
// 0.6
let env = build_env([provide_reqwest_http_client()])?;
let res = run_async(execute::<Env>(HttpRequest::get(url)), env).await?;

let mut env = Env::new();
env_set_http_client(&mut env, Arc::new(Canned));
let res = run_blocking(execute::<Env>(HttpRequest::get(url)), env)?;

// 0.7
let program = execute::<HttpClientEnv, _>(HttpRequest::get(url))
  .provide_layer(ReqwestHttpClientProvider::layer());
let exit = TokioRuntime::current()?.run(program).await;

let exit = execute(HttpRequest::get(url))
  .provide_all(http_client_env(Canned))
  .block_on();
```

`auth` (`SessionStore`, `OAuthClient`, the memory doubles) is ported the same way: the methods
return `next::Effect<_, _>`.
