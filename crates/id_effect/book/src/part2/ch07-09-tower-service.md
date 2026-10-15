# Tower service (`id_effect_axum::service`)

The **`service`** module of **`id_effect_axum`** (feature **`tower`**) implements
**[`tower::Service`](https://docs.rs/tower)** for **`Effect`**-based handlers. Each call runs as one
request fiber on the `EffectExecutor`, so you can compose **Tower middleware** (timeouts, retries,
load balancing, …) around the same domain style as Axum, without Axum's router.

## When to use it

- You already have a **Tower stack** (custom servers, gRPC gateways, middleware chains) and want
  **`poll_ready` / `call`** semantics.
- You need **per-service concurrency limits** or **request metrics** at the Tower boundary.

## `EffectService`

```rust,ignore
use id_effect::succeed;
use id_effect_axum::service::EffectService;
use tower::{Service, ServiceExt};

let mut svc = EffectService::new((), |_env: &mut (), x: u32| {
    succeed::<u32, (), _>(x.saturating_add(1))
});
let out = svc.ready().await.unwrap().call(41).await.unwrap();
assert_eq!(out, 42);
```

- **`EffectService::new(state, f)`** — `f(&mut state, request)` returns an `Effect`; unlimited
  concurrency. The service's error type is the whole **`Cause<E>`**: a typed failure is
  `Cause::Fail(e)`, a handler panic `Cause::Die(..)`, an interrupt `Cause::Interrupt(..)`.
- **`EffectService::with_max_in_flight(state, n, f)`** — a semaphore gates `poll_ready` so at most
  *n* handler effects run at once; `in_flight_counter()` exposes the count. The permit and the
  count are released by a finalizer of the request fiber, on every exit path.
- **`.with_request_metrics(latency: Histogram, errors: Counter)`** — records each call's duration
  (seconds) and counts typed failures.
- **`.with_executor(executor)`** — run on a specific `EffectExecutor` instead of the global one.

Response futures are `Send`; any Tokio runtime flavour works (the example above runs on
`current_thread`). `ChannelService::new(channel)` serves requests over an `id_effect::Channel`.

## Further reading

- `cargo run -p id_effect_axum --features tower --example 050_tower_effect_service` (and `051_tower_shared_counter`)
- `cargo doc --open -p id_effect_axum --features tower`
- [Axum host](./ch07-08-axum-host.md) if you are on Axum specifically.
