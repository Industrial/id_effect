# Labs `id_effect_rpc` and `id_effect_ai` on the 0.7 kernel

0.7 moves the labs crates `id_effect_rpc` and `id_effect_ai` to the new kernel.
This is a kernel migration only (R5): no items are added or removed, and the
public signatures differ only in their kernel types (plus the `Send`/`Sync` bounds and the
`Selector` index parameter the 0.7 kernel needs). `id_effect::schema` is kernel-neutral and
unchanged. Neither crate depends on `tokio` any more (it stays a dev-dependency), and the
temporary 0.7 bridges are gone: `run_v1_handler` (rpc, m-axum), `next_http_runtime` /
`into_v1_on` (ai and the rpc client, m-platform) and the `IntoNext` adapters in the ai examples
(m-cli).

## `id_effect_rpc`

| item | 0.6 | 0.7 |
|---|---|---|
| `RpcGroup::register(.., handler)` | `handler: Fn(A, &mut Env) -> v1 Effect<B, RpcError, Env>` | `handler: Fn(A, &mut DynEnv) -> Effect<B, RpcError, DynEnv>` |
| `RpcMethodEntry::dispatch(payload, env)` | `env: Env` | `env: DynEnv` |
| `RpcServer::{mount_dispatch, mount_default}`, `layer_rpc` | `Router<Env>` | `Router<DynEnv>` |
| `RpcClient::call::<.., R>(..)` | `R: Needs<HttpClientService>` → v1 `Effect<B, RpcClientError, R>` | `call::<.., R, I>(..)`, `R: Selector<HttpClientService, I> + HList` → `Effect<B, RpcClientError, R>` |
| `codegen::emit_service_trait` output | `use id_effect::Effect;` … `-> Effect<T, RpcError, ()>` | `use id_effect::Effect;` … `-> Effect<T, RpcError>` |

**Dispatch.** A handler's effect runs as one request fiber on the tokio runtime serving the
request (`id_effect_axum::run_with_env`), instead of on tokio's blocking pool. A typed
`RpcError` still becomes a failure envelope. **Behaviour change:** a defect (a panic in the
handler or its effect) or an interrupt now becomes an `RpcError::internal` failure envelope
(HTTP 500, the panic message is not sent); 0.6 re-raised the panic in the axum handler.

```rust
// 0.6
use id_effect::{Env, schema, succeed};
group.register("greet", payload, &["name", "n"], success, &["message", "n"],
  |(name, n), _env: &mut Env| succeed((format!("Hello, {name}!"), n)));
let app = RpcServer::new(group).mount_default().with_state(Env::new());
let out = run_async(client.call("greet", input, &p, &pf, &s, &sf), env).await?;

// 0.7
use id_effect::{DynEnv, succeed};
use id_effect::schema;
use id_effect_platform::http::{ReqwestHttpClient, http_client_env};
group.register("greet", payload, &["name", "n"], success, &["message", "n"],
  |(name, n), _env: &mut DynEnv| succeed((format!("Hello, {name}!"), n)));
let app = RpcServer::new(group).mount_default().with_state(DynEnv::new());
let out = client
  .call("greet", input, &p, &pf, &s, &sf)
  .provide_all(http_client_env(ReqwestHttpClient::default_client()))
  .block_on() // or TokioRuntime::current()?.run(..).await in async code
  .into_result()?;
```

## `id_effect_ai`

| item | 0.6 | 0.7 |
|---|---|---|
| `LanguageModel::{complete, complete_stream}` | v1 `Effect<_, AiError, ()>`; stream = v1 `Stream<CompletionChunk, AiError, ()>` | `Effect<_, AiError>`; stream = `Stream<CompletionChunk, AiError>` |
| `complete::<R>`, `complete_stream::<R>` | `R: Needs<LanguageModelService>` | `complete::<R, I>`, `R: Selector<LanguageModelService, I> + HList` |
| `mock_chunk_stream` | v1 `Stream` fed by a thread (`run_blocking` per chunk) | `Stream::from_vec` (no thread) |
| `provide_mock_language_model()` | `ProviderBox` | `provide_mock_language_model::<E>() -> Layer<HCons<LanguageModelService, HNil>, E>` |
| `provide_openai_language_model(client, cfg)`, `provide_anthropic_language_model(..)` | `Result<ProviderBox, AiError>` | `::<E>(..) -> Result<Layer<HCons<LanguageModelService, HNil>, E>, AiError>` |
| `provide_cursor_agents_client(client, cfg)` | `Result<ProviderBox, AiError>` | `::<E>(..) -> Result<Layer<HCons<CursorAgentsClientService, HNil>, E>, AiError>` |
| `CursorAgentsClient` methods, `cursor::models::list_models` | v1 `Effect<_, AiError, ()>` | `Effect<_, AiError>` |
| `default_ai_retry_schedule()` | v1 `Schedule` | `Schedule<(Duration, u64), AiError>` (`exponential(50ms).intersect(recurs(3))`) |
| `retry_transient_ai_http(make)` | `make: Fn() -> v1 Effect + Send + Clone` | `make: Fn() -> Effect + Send + Sync` (`Effect::retry` with the schedule `while_input(AiError::is_transient)`) |
| `with_ai_request_span`, `with_ai_span` | v1 `with_span` | `Effect::with_span("ai.request")` (the fiber's `SpanHook`, e.g. id_effect_opentelemetry); `R: Send + Sync` |

`LanguageModelService` / `CursorAgentsClientService` stay `Arc<dyn …>` and are the capabilities in
the typed environment. The HTTP adapters call `id_effect_platform`'s effects
directly, so they run on any runtime with a tokio IO driver: `block_on` /
`Runtime::global()` (all tokio drivers since df5aa10) or `id_effect_tokio::TokioRuntime`.

**Behaviour changes.** `retry_transient_ai_http` now waits the schedule's delays (50, 100,
200 ms) on the fiber's clock; 0.6 parked the thread for `25 ms × attempt` and ignored the
schedule's delays. `wait_until_idle` polls with `sleep` (200 ms) instead of
`tokio::time::sleep`. The SSE adapters build their chunk stream from the buffered body directly
(same chunks, same final `done` chunk) instead of feeding it from a thread.

```rust
// 0.6
let env = build_env([provide_openai_language_model(client, cfg)?])?;
let resp = run_async(complete(req), env).await?;
let chunks = run_blocking(model.complete_stream(req).flat_map(|s| s.run_collect()), ())?;

// 0.7
use id_effect::{HCons, HNil};
let layer = provide_openai_language_model::<AiError>(client, cfg)?;
let resp = complete::<HCons<LanguageModelService, HNil>, _>(req)
  .provide_layer(layer)
  .block_on() // or TokioRuntime::current()?.run(..).await
  .into_result()?; // Result<_, Cause<AiError>>; `cause.fail_option()` is the typed error
let chunks = model
  .complete_stream(req)
  .flat_map(|s| s.run_collect())
  .block_on()
  .into_result()?;
```

## Tests and examples

- The wiremock tests run their effects on the test's tokio runtime
  (`TokioRuntime::current()?.run(..)`); the sync tests use `block_on`.
- `010_openai_once` / `011_anthropic_once` are plain `fn main` programs that `block_on` the
  effect; `ask_once` / `020_cursor_agent_once` pass the native effect to `run_main`.
- `id_effect_rpc` gains a registry test for the typed-failure and defect envelopes.
