# Parallelism — Compute Fabric and Fibers

There are two kinds of parallelism in `id_effect`, and they are controlled differently:

| Workload | API | Parallelism |
|----------|-----|-------------|
| Pure bulk work on a chunk (`Stream::map`, `Stream::filter`) | primary method name | Rayon per chunk, when the Compute Fabric says it pays off (feature `compute`) |
| Pure, ordered or `FnMut` work | `map_serial`, `filter_serial` | none: in order on the pulling fiber |
| Effectful stream steps | `map_effect` / `map_effect_par(n, f)` | sequential / at most `n` child fibers |
| Independent effects | `all`, `for_each_par`, `zip_par`, `~(a, b)` in `effect!` | child fibers under a `Concurrency` policy |

## Pure bulk work: Rayon through the Fabric

`Stream::map` and `Stream::filter` take `Fn + Send + Sync` closures and apply them per upstream
chunk. With the `compute` feature (on by default), each chunk asks
`id_effect::compute::should_parallelize(len)` (the default threshold adjusted by the current
`AdaptiveContext`) and runs on Rayon via `install_parallel` when it says yes. Output order is
preserved either way. Without the feature, bulk operations run serially and `rayon` / `sysinfo`
are not built.

```rust,ignore
use id_effect::Stream;

let evens = Stream::<i64, ()>::range(0, 10_000)
  .map(|n| n * 2)
  .filter(|n: &i64| n % 4 == 0)
  .run_collect();
```

Use `map_serial` / `filter_serial` for `FnMut` closures with captured mutable state or
order-sensitive side effects (example `071_stream_map_serial`):

```rust,ignore
let mut offset = 0;
let s = Stream::<i32, ()>::from_iterable(1..=8).map_serial(move |n| {
  offset += 1;
  n + offset
});
```

The same rule applies to the bulk `Vec` helpers in `id_effect_fp`
(`id_effect_fp::algebra::functor::vec::map` / `map_serial`) and to `order::sort_with` /
`sort_with_serial` in `id_effect::schema::order`.

## Effectful stream steps

`map_effect(f)` is sequential: one element's effect finishes before the next starts, so side
effects happen in stream order. `map_effect_par(n, f)` runs at most `max(n, 1)` effects at once as
child fibers of the pulling fiber; output is in stream order, side-effect order is not, and the
first failure interrupts the chunk's in-flight effects (example `070_stream_map_par_n`).

```rust,ignore
let fetched = Stream::<u32, ()>::from_iterable(0..8).map_effect_par(4, |n| succeed(n * 2));
```

## Fiber concurrency and the Fabric's admission

`all(effects, concurrency)`, `for_each_par` and `zip_par` fork child fibers under a
`Concurrency` policy:

- `Concurrency::Unbounded` starts every child at once;
- `Concurrency::Bounded(n)` runs at most `n`;
- `Concurrency::Inherit` awaits a permit from the ambient `Fabric` before each child starts
  (with no ambient fabric it behaves like `Unbounded`).

Provide a fabric around an effect with `Fabric::with_capacity(n).provide(eff)`, or put a Compute
Fabric's telemetry-driven admission in front of fibers with `Fabric::from_compute(fabric)`. Under
memory pressure the governor lowers admission, so fewer `Inherit` children run at once
(examples `120_compute_fabric_memory_cap`, `122_compute_fabric_effect_parallel`):

```rust,ignore
use std::sync::Arc;
use id_effect::compute::ComputeFabric;
use id_effect::{Concurrency, Effect, Fabric, all, succeed};

let fabric = Arc::new(ComputeFabric::memory_cap_max_cpu(0.85));
fabric.governor().tick();
let work: Vec<Effect<u32, ()>> = (0..4).map(|_| succeed(42)).collect();
let out = Fabric::from_compute(fabric)
  .provide(all(work, Concurrency::Inherit))
  .block_on();
```

Inside `effect!`, `~(a, b)` joins two sub-effects in parallel; plain `~a; ~b` binds run one after
the other. See [Compute Fabric](../part3/ch12-00-compute-fabric.md) for the admission model and
the governor.

See also [ADR 0008](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0008-implicit-parallelism.md)
(implicit parallelism; Amendment 2 makes `map_effect` sequential).
