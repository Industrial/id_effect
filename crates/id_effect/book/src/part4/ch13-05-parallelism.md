# Implicit Parallelism — Compute Fabric

Bulk transforms and effectful stream steps parallelize **implicitly** through [**Compute Fabric**](../part3/ch12-00-compute-fabric.md). There is no public `Parallelism` type and no `*_with` / `*_par` policy surface — Fabric decides when Rayon or bounded effect concurrency is profitable.

Use the **primary method name** (`map`, `filter`, `sort_with`, …). Opt out with `*_serial` when you need `FnMut`, non-`Send` closures, or deterministic ordering.

## How dispatch works

Every `run_blocking` / `run_async` / `run_with` boundary refreshes the thread-local [`AdaptiveContext`](../src/compute/adaptive.rs) from the installed supervisor. Bulk paths call [`parallel_if_profitable`](../src/compute/dispatch.rs), which consults `should_parallelize_current(len)` and runs Rayon via [`install_parallel`](../src/compute/rayon_pool.rs) when the snapshot says it pays off.

| Workload | API | Parallelism model |
|----------|-----|-------------------|
| Pure bulk (`Vec`, `HashMap`, stream chunks, …) | `map`, `filter`, `sort_with`, … | Rayon per chunk when profitable |
| Effectful stream | [`Stream::map_effect`](../src/streaming/stream.rs) | Admission-bounded async concurrency per chunk |
| `effect!` binds | primary `~` syntax | EDG parallelizes **independent** bind sets |
| Deterministic tests | `*_serial`, `#[effect(serial)]` | explicit opt-out |

Under memory pressure the supervisor lowers admission and raises the element threshold; under headroom it admits more fibers and parallelizes smaller chunks. See example `120_compute_fabric_memory_cap.rs`.

## Collections and `vec`

```rust
use id_effect::vec;

let doubled = vec::map(vec![1, 2, 3], |x| x * 2);
// Fabric may use Rayon when len is large enough and headroom allows.
```

Escape hatch for captured mutable state or non-`Send` closures:

```rust
use id_effect::vec;

let mut acc = 0;
vec::map_serial(v, |x| { acc += x; acc });
```

The same pattern applies to `HashMap::map_values`, `filter`, red-black tree scans, `sort_with`, and similar bulk APIs.

## Streams — pure transforms

`Stream::map` and `Stream::filter` apply Fabric-aware Rayon **per upstream chunk**:

```rust
use id_effect::Stream;

Stream::from_iterable(0..10_000)
    .map(|n| n * 2)
    .filter(|n| *n % 2 == 0)
    .run_collect();
```

For `FnMut` or ordering-sensitive work, use `map_serial` / `filter_serial`:

```rust
let mut offset = 0;
stream.map_serial(move |n| { offset += 1; n + offset });
```

See example `071_stream_map_serial.rs`.

## Streams — effectful steps

When each element runs an `Effect`, use [`Stream::map_effect`](../src/streaming/stream.rs). Concurrency is capped by the current admission budget (at least one permit):

```rust
use id_effect::{Stream, succeed};

Stream::from_iterable(items)
    .map_effect(|item| process(item))
    .run_collect();
```

Output order matches stream order. Each mapper receives a **clone** of the environment `R`, so `R` must be `Clone + Send + Sync`.

With Compute Fabric installed, a memory cap or throttle directly reduces how many effect mappers run in flight. See example `122_compute_fabric_effect_parallel.rs`.

## `effect!` and the Effect Dependency Graph

Independent `~` steps with no data or capability conflict may run concurrently when the EDG finds a parallel bind set. Dependent steps and overlapping capability borrows stay sequential.

Opt out per block:

```rust
#[effect(serial)]
effect! { |r| {
    ~step_a();
    ~step_b();
}}
```

Example: `122_compute_fabric_effect_parallel.rs` (stream effect mapping under Fabric). For multi-step programs, see [Compute Fabric](../part3/ch12-00-compute-fabric.md) § Effect binds.

## Migration from 0.3.x

| Old (0.3.x) | New (0.4.0) |
|-------------|-------------|
| `Parallelism::Auto / ForceParallel / Serial` | removed — Fabric decides |
| `map_with(Parallelism::…, f)` | `map(f)` or `map_serial(f)` |
| `*_par` (deprecated) | `map(f)` |
| `Stream::map_par_n(n, f)` | `map_effect(f)` |
| `Stream::map_par_adaptive(f)` | `map_effect(f)` |
| `compute::effective_threshold` | removed (internal to Fabric) |

See [ADR 0008](../../../../docs/adrs/0008-implicit-parallelism.md) (supersedes the public surface of [ADR 0006](../../../../docs/adrs/0006-parallel-by-default.md)).
