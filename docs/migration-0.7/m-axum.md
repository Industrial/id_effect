# m-axum: `id_effect_axum` on the 0.7 kernel

Handlers are `id_effect::next::Effect`s. Because `next` effects are `Send`, every request's effect
now runs as **one fiber on the tokio runtime that serves the request** (a `TokioExecutor` over the
ambient handle). The 0.6 `EffectExecutor` thread pool (one OS thread with a private
`current_thread` runtime and a `LocalSet` per in-flight request) is gone, and so is every
`block_in_place` / `run_blocking` bridge in the crate.

## What changed

| 0.6 | 0.7 |
|---|---|
| handler `FnOnce(&mut S) -> id_effect::Effect<A, E, S>` | `FnOnce(&mut S) -> id_effect::next::Effect<A, E, S>`; `S: Send + Sync` (was `Send`). `S` can be plain state, an HList of capabilities (`need::<Db>()`), or a `DynEnv` |
| `run_with_env` / `run_with_caps` / `run_with_request_caps` → `Result<A, E>`, a panic re-raised in the caller | → `Exit<A, E>` (`into_result()` for `Result<A, Cause<E>>`); a panic is `Cause::Die` |
| `execute` / `routing::*`: `Err(e)` → `e.into_response()`, a panic unwound the handler | typed failure → `E::into_response()`; **defect (any panic in the build closure or the effect) → `500`** with a `HandlerDefect { message }` response extension (the message is never sent to the client); interrupt → `503`. `exit_response` / `cause_response` are public |
| client disconnect: the effect ran to completion (`DropPolicy::RunToCompletion`, default) or was dropped at its next `.await` (`DropPolicy::Cancel`) | the request fiber is **interrupted** (`DropPolicy::Interrupt`, default): its finalizers (`ensuring`, `acquire_release`, scoped layers) run. `DropPolicy::Detach` keeps the old run-to-completion behaviour |
| `EffectExecutorBuilder::{max_threads, queue_capacity, idle_timeout}`, `EffectExecutor::threads`, `run_local` / `try_run_local` | removed. `max_in_flight(n)` (unbounded by default) + `SaturationPolicy::{Wait, Reject}` (503) remain; new `runtime(rt)` pins a `Runtime`/`TokioRuntime`; `in_flight()`; `run` / `try_run` return `Exit`; `run_effect` / `try_run_effect` run an `Effect<A, E, HNil>` |
| request caps over v1 `Env` + `CapabilityKey` (`CapOverrideLayer::<Cap<Tenant>, _>`) | over `next::DynEnv`, keyed by type: `CapOverrideLayer::new(\|parts\| Some(Tenant(..)))`, `RequestCaps::{insert, insert_arc, get}`; state is `State<DynEnv>` |
| `routing::*_with_metrics(Metric<u64, ()>, Metric<Duration, ()>, f)` | `(next::Counter, next::Histogram, f)`; the histogram records seconds on the executor clock (failures are timed too) |
| `channel_bridge::{exchange, exchange_into_response(env, QueueChannel, req)}` | over `next::Channel<Response<Bytes>, Request<Bytes>>`; `exchange_into_response(ch, req)` (no env) |
| `service::EffectService` (`type Error = E`), `with_request_metrics(Metric, Metric)` | `type Error = Cause<E>` (a handler panic is `Cause::Die`); `with_request_metrics(Histogram, Counter)`; `S: Clone + Send + Sync`; in-flight count and permit released by a fiber finalizer |
| `service::ChannelService<S, Req, Res>::new(state, QueueChannel)` | `ChannelService<Req, Res>::new(next::Channel<Res, Req>)` |
| `server::{load_host_config::<R: Needs<..>>() -> v1 Effect, provide_host_config_env() -> ProviderBox, bootstrap_env(p) -> Env}`; `Host::env: Env` | `load_host_config::<R, I>()` → `next::Effect` (`R: Selector<ConfigProviderService, I>`); `provide_host_config_env::<E>()` → `Layer<ConfigEnv, E>`; `bootstrap_env(p)` → `ConfigEnv`; `HostConfig::{config(), layer::<E>()}`; `Host::env: ConfigEnv`; `HostError::Defect` |
| — | the server as a scoped layer: `server_layer(app)` (binds on acquire, graceful shutdown within `shutdown_timeout` on release, provides `ServerHandle { local_addr() }`), `serve(app)` (= `launch`), `serve_until(app, stop)`, `serve_until_shutdown(app)` (SIGINT/SIGTERM) |
| — | `body::{stream_body, effect_body, body_stream}`: response bodies from a `next::Stream<Bytes, E>` (dropping the body interrupts the producer fiber), request bodies as a stream |

Unchanged: `json`, `health`, `server::{security, shutdown}`, the `cluster` routes, `serve_router`,
`HostBuilder::run_until_shutdown`, the `tower` feature (`service` module), and the
`gate/axum_request_oneshot` / `gate/axum_request_caps` bench IDs. `id_effect_tokio` is no longer a
dependency (a `TokioRuntime` converts into the `Runtime` that `EffectExecutorBuilder::runtime`
takes).

## Before / after

```rust
// 0.6
use id_effect::{Cap, Effect, Env, succeed};

fn whoami(env: &mut Env) -> Effect<String, Infallible, Env> {
  succeed(env.get::<Cap<Tenant>>().0.clone())
}
let app = Router::new()
  .route("/whoami", get(|st: State<Env>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)))
  .layer(CapOverrideLayer::<Cap<Tenant>, _>::new(|p| Some(Tenant(tenant_of(p)))))
  .with_state(base_env);
let n: Result<u32, E> = run_with_env(state, |s| count(s)).await;
```

```rust
// 0.7
use id_effect::next::{DynEnv, Effect, HCons, HNil, need, succeed};

// Typed capabilities as router state:
type AppEnv = HCons<Db, HNil>;
fn users(_: &mut AppEnv) -> Effect<Json<Vec<User>>, ApiError, AppEnv> {
  need::<Db>().flat_map(|db| db.list_users()).map(Json)
}
let app = Router::new().route("/users", routing::get(users)).with_state(hlist![db]);

// Runtime-checked request caps:
fn whoami(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  succeed(env.try_get::<Tenant>().map_or("none".into(), |t| t.0.clone()))
}
let app = Router::new()
  .route("/whoami", get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)))
  .layer(CapOverrideLayer::new(|p| Some(Tenant(tenant_of(p)))))
  .with_state(DynEnv::new().with(Tenant("public".into())));
let n: Result<u32, Cause<E>> = run_with_env(state, |s| count(s)).await.into_result();

// The server's lifetime is a scoped layer:
let config = provide_host_config_env::<HostError>().provide_to(HostConfig::layer());
let program = serve_until_shutdown(app).provide_layer(config);
let exit = EffectExecutor::global().run_effect(program).await;
```

## Dependents

`labs/id_effect_rpc` (`RpcGroup::register`) used `run_with_env` with its v1 `Env` handlers. Its
call site now drives the v1 effect on tokio's blocking pool (`run_v1_handler` in `registry.rs`,
adding a `tokio` (`rt`) dependency) until `l-labs-edge` switches rpc to `next`. The rpc example's
`json::decode_json_schema` is unchanged.
