---
name: id_effect-streams
description: >-
  Teaches id_effect streams: Stream vs Effect, chunks, sinks, backpressure,
  map_par_n async concurrency, and Rayon Parallelism policy. Use when processing
  iterables, pipelines, bulk transforms, or choosing serial vs parallel collection ops.
---

# id_effect Streams

**Part IV ch13** + ADR 0006 (parallel-by-default).

**Prerequisite**: `id_effect-fundamentals`.

## Stream vs Effect

- **`Effect<A, E, R>`** — one outcome.
- **`Stream<A, E, R>`** — many elements over time; pull-based with backpressure.

```rust
Stream::from_iterable(0..10_000)
    .map(|n| n * 2)
    .filter(|n| *n % 2 == 0)
    .run_collect();
```

## Chunks & sinks

- **Chunks** — batch upstream elements for efficient processing.
- **Sinks** — consume streams (`run`, `run_collect`, `run_drain`).
- **Backpressure** — slow consumers limit upstream pull rate (ch13-03).

## Parallelism — Rayon (pure transforms)

**`Effect` and `effect!` stay sequential.** Bulk **pure** collection/stream chunk ops use Rayon when large enough.

```rust
use id_effect::{Parallelism, vec};

vec::map(v, |x| x * 2);  // Auto: parallel when len >= 1024

vec::map_with(Parallelism::ForceParallel, v, f);
vec::map_serial(v, g);   // FnMut / non-Send escape hatch
```

| Policy | When Rayon runs |
|--------|-----------------|
| `Auto { threshold: 1024 }` (default) | `len >= threshold` |
| `ForceParallel` | always |
| `Serial` | never |

Deprecated `*_par` → `ForceParallel` via `*_with` or primary methods.

## `map_par_n` — async effect concurrency

**Not Rayon.** Bounded concurrent **effectful** steps:

```rust
stream.map_par_n(8, |item| process(item))  // at most 8 effects in flight
```

Use when each element runs an `Effect`; use Rayon bulk APIs for pure transforms.

## Not this → but that

| Not this | But that |
|----------|----------|
| Parallel `~` binds in `effect!` | `fiber_all` or `map_par_n` |
| `map_par` (deprecated) | `map` or `map_with(Parallelism::ForceParallel, …)` |
| Rayon for IO-bound effects | `map_par_n` with bounded concurrency |
| Collecting unbounded streams without sink | explicit sink + backpressure policy |


## Advanced streaming (Part V ch22)

Modules under `id_effect::streaming`:

| Combinator | Module | Use |
|------------|--------|-----|
| `tumbling` / `sliding` / `session_by_gap` | `window` | Count/time/session windows → `Stream<Vec<A>>` |
| `merge` | `join` | Fair element interleave of two streams |
| `combine_latest` | `join` | Latest pair when either side updates |
| `keyed_join` | `join` | Inner join on latest per key |
| `broadcast_with_replay` | `replay` | Fanout + replay tail buffer |
| `state_scan` | `state_scan` | FSM step with optional output |
| `via_transducer` | `transducer` | Optics-compatible map/filter pipeline |

```rust
use id_effect::{Stream, combine_latest, state_scan, transducer_map, Transducer};

Stream::from_iterable(events).tumbling(100);
combine_latest(temperatures, setpoints);
Stream::from_iterable(inputs).via_transducer(transducer_map(|x| x * 2));
```

Transducers mirror `id_effect_optics::Transducer` but live in `id_effect` to avoid a dependency cycle.

## Verify

```bash
cargo test -p id_effect -- examples::071_
```

## Next

- Schema at stream boundaries: [id_effect-schema](../id_effect-schema/SKILL.md)
