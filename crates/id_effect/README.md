# `id_effect`

`Effect<A, E = Never, R = HNil>`: a lazy, `Send` effect over a typed capability list `R`, run on a
fiber runtime (Tokio executor by default), with layers, streams, schedules and coordination.
Tier: **core** ([ADR 0010](../../docs/adrs/0010-crate-tiers.md)); kernel design A
([ADR 0011](../../docs/adrs/0011-fiber-runtime-send-typed-r.md)).

## Quick start

```rust
use id_effect::{Effect, Exit, caps, effect, fail, succeed};

#[derive(Debug)]
struct BasePort(u16);

fn parse_port(raw: &str) -> Effect<u16, String> {
  match raw.parse::<u16>() {
    Ok(port) => succeed(port),
    Err(e) => fail(format!("bad port {raw:?}: {e}")),
  }
}

fn next_port(raw: &str) -> Effect<u16, String, caps!(BasePort)> {
  let parsed = parse_port(raw);
  effect!({ ~parsed + ~BasePort.0 })
}

let ok = next_port("8080").provide(BasePort(1)).block_on();
assert_eq!(ok, Exit::succeed(8081));
assert!(next_port("http").provide(BasePort(1)).block_on().into_result().is_err());
```

- `succeed` / `fail` / `sync` / `from_async` build effects; `map`, `flat_map`, `map_err`,
  `catch`, `zip`, `ensuring` combine them.
- `effect!({ .. })` is do-notation over an async body: `~Cap` reads a capability, `~expr` binds a
  sub-effect (widening its error), `~(a, b)` joins effects in parallel, `?` fails with a
  `Result`'s error. `effect_local!` builds a `LocalEffect` (`!Send` bodies).
- `R` is an HList (`caps!(A, B)` = `HCons<A, HCons<B, HNil>>`). Provide it with `.provide(v)`,
  `.provide_env(hlist![..])` or a `Layer` (`.provide_layer(layer)`); a missing capability is a
  compile error.
- Run at the edge with `Effect::block_on`, or `Runtime::run(eff).await` inside async code.

Cargo features: `compute` (default — the Compute Fabric substrate, adaptive Rayon bulk ops,
`sysinfo` telemetry), `schema-serde`, `schema-regex` (regex-backed `schema::regex` validator),
`proptest` (`testing::property`). `--no-default-features` drops `rayon` and `sysinfo`.
Algebraic type classes, persistent (`im`) collections and STM live in the maintenance-tier
[`id_effect_fp`](../id_effect_fp) crate. Examples: [`examples/README.md`](examples/README.md).

---

## Construct catalog (`src/`)

Every module's items are re-exported at the crate root (`use id_effect::{Effect, Layer, Stream, …}`).

| Module | What lives there |
|--------|------------------|
| `kernel` | `Effect`, `LocalEffect`, constructors (`succeed`, `fail`, `die`, `sync`, `suspend`, `from_async`, `sleep`, `yield_fiber`), `Ctx`, the runloop, `ensuring` / masks / `acquire_use_release`, `block_on` edges, `ExitKind` |
| `capability` | typed `R`: `HCons`, `HNil`, `hlist!`, `Selector` / `Plucker` / `Sculptor` / `Embed`, `need`, `provide*`, `narrow`, `DynEnv` |
| `layer` | `Layer<Out, E, In>`: `succeed`, `effect`, `scoped`, `acquire_release`, `merge`, `provide_to`, `provide_merge`, `fresh`, `launch` |
| `runtime` | `Runtime` (`global`, `run`, `block_on`, `fork`), `Executor`, `TokioExecutor`, fibers (`FiberHandle`, `FiberContext`), `FiberRef`, `run_fiber` |
| `concurrency` / `par` / `race` | `fork_all` + `Concurrency`, `all`, `for_each_par`, `zip_par`, `race`, `race_all`, `race_first`, `timeout` |
| `scope` | `Scope` (finalizers, `fork_in`), `Effect::scoped` |
| `coordination` | `Queue`, `Channel`, `Deferred`, `Latch`, `PubSub`, `Ref`, `SynchronizedRef`, `SubscriptionRef`, `Semaphore` / `Permit`, `Supervisor` |
| `resource` | `Pool`, `KeyedPool`, `Cache` / `CacheStats` |
| `streaming` | `Stream` (pull, chunked), `Sink`, `StreamSender` / `stream_from_channel*`, backpressure, broadcast, windows, `Chunk`, `Transducer` |
| `scheduling` | `Schedule` (`recurs`, `spaced`, `exponential`, …) for `Effect::retry` / `repeat`; `Duration`, `UtcDateTime`, `ZonedDateTime` |
| `observability` | `LogLevel`, metrics (`Metric`, `MetricRegistry`), spans (`with_span`, `SpanHook`), the in-process tracing buffer (`install_tracing_layer`, `snapshot_tracing`, `tracing_hook`) |
| `request` | `Request`, `RequestResolver` (batching), request cache |
| `random` / `testing` | `Random`; the deterministic `TestExecutor` (virtual time), `TestRandom` |
| `compute` | the kernel's admission `Fabric`; with `compute`: `ComputeFabric`, `AdmissionController`, `FabricGovernor`, adaptive Rayon dispatch |
| `failure` | `Cause`, `Exit`, `Defect`, `FiberId`, `Never`, `Or` |
| `foundation` | `Either`, `Pipe` (+ `pipe!`), `Predicate`, `identity` / `always` / `compose` |
| `schema` | `Schema`, `Unknown`, `ParseError(s)`, `HasSchema` (+ `derive(Schema)`), `EffectData`, `Brand`, `Equal`, `Ordering`, `Redacted` |
| `match_` | `Matcher`, `HasTag` (+ `match_effect!`, `#[effect_tagged]`) |
| `macros` | `effect!`, `effect_local!`, `caps!`, `pipe!`, `err!` |

The 0.6 kernel (runtime capability DI with `Env` / `ProviderSpec` / `run_with`, `run_blocking`,
`ThreadSleepRuntime`, `CancellationToken`, `TestClock`, `run_test`) was removed in 0.7; see
[`docs/MIGRATION-0.7.md`](../../docs/MIGRATION-0.7.md).
