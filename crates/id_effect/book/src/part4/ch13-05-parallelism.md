# Parallelism — Rayon by Default

Bulk **pure** transforms on collections and stream chunks use Rayon when the input is large enough. **`Effect` and `effect!` stay sequential** for capability borrows — except that the Effect Dependency Graph may parallelize **independent** `~` binds when no data dependency exists (see [Compute Fabric](../part3/ch12-00-compute-fabric.md)).

When [**Compute Fabric**](../part3/ch12-00-compute-fabric.md) is installed, [`Parallelism::Auto`](../src/parallelism.rs) uses [`AdaptiveContext`](../src/compute/adaptive.rs) instead of the fixed 1024 threshold, and [`Stream::map_par_adaptive`](../src/streaming/stream.rs) caps effect concurrency from the admission budget.

## `Parallelism` policy

```rust
use id_effect::Parallelism;

// Default: parallel when len >= 1024
Parallelism::default();

// Always Rayon (old `*_par` behavior)
Parallelism::ForceParallel;

// Never Rayon (old primary `map` / `filter` behavior)
Parallelism::Serial;
```

| Policy | When Rayon runs |
|--------|-----------------|
| `Auto { threshold: 1024 }` | `len >= threshold` |
| `ForceParallel` | always |
| `Serial` | never |

## Collections and `vec`

Primary methods dispatch through the default policy:

```rust
use id_effect::vec;

let doubled = vec::map(vec![1, 2, 3], |x| x * 2);
```

Use explicit control when needed:

```rust
use id_effect::{Parallelism, vec};

// Force parallel on small inputs (benchmarks, tests)
vec::map_with(Parallelism::ForceParallel, v, |x| f(x));

// Non-`Send` or `FnMut` closure — serial escape hatch
vec::map_serial(v, |x| g(x));
```

The same pattern applies to `HashMap::map_values`, `filter`, red-black tree scans, `sort_with`, and similar bulk APIs: primary name, `*_serial`, `*_with(policy, …)`.

## Streams

`Stream::map` and `Stream::filter` apply the default policy **per chunk** after pulling upstream:

```rust
use id_effect::{Parallelism, Stream};

Stream::from_iterable(0..10_000)
    .map(|n| n * 2)                    // Auto threshold on chunk len
    .filter(|n| *n % 2 == 0)
    .run_collect();
```

```rust
// Explicit policy per operator
stream.map_with(Parallelism::Serial, |n| expensive(n));

// Captured mutable state — must use serial
let mut acc = 0;
stream.map_serial(move |n| { acc += n; acc });
```

### Not the same as `map_par_n`

[`Stream::map_par_n`](./ch13-03-backpressure.md) is **bounded async concurrency** for effectful steps — it does not use Rayon. Use it when each element runs an `Effect` and you want at most `n` in flight.

## Migration from `*_par`

Deprecated `*_par` methods are aliases to `ForceParallel`. Prefer:

| Old | New |
|-----|-----|
| `map_par(f)` | `map(f)` or `map_with(Parallelism::ForceParallel, f)` |
| `filter_par(p)` | `filter(p)` or `filter_with(Parallelism::ForceParallel, p)` |
| serial `map` / `filter` (pre-3.x) | `map_serial` / `filter_serial` |

See [ADR 0006](../../../../docs/adrs/0006-parallel-by-default.md) and example `071_stream_map_serial.rs`. For hardware-aware thresholds and `map_par_adaptive`, see [Compute Fabric](../part3/ch12-00-compute-fabric.md) and example `122_compute_fabric_effect_parallel.rs`.
