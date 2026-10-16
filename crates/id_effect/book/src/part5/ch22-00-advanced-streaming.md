# Advanced Streaming

**Part V · Chapter 22** — windowing, joins, replay fanout, FSM scans, and transducers.

Part IV introduced the pull-based, chunked [`Stream<A, E = Never, R = HNil>`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/stream.rs). This chapter covers **multi-stream** patterns: grouping elements into windows, joining sources, replaying history to fanout branches, stepping simple state machines over elements, and applying composable transducers. Every operator pulls one upstream chunk per downstream pull, so memory stays bounded by the chunk size plus the operator's documented state.

## Module map

All items are re-exported at the crate root (`use id_effect::{Stream, combine_latest, …}`).

| Source | Items |
|--------|-------|
| [`window.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/window.rs) | `tumbling`, `sliding`, `session_by_gap`, `tumbling_by_time`, `sliding_by_time` |
| [`join.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/join.rs) | `Stream::merge`, `combine_latest`, `keyed_join` |
| [`zip.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/zip.rs) | `Stream::zip`, `Stream::zip_with`, `Stream::combine_latest` |
| [`time.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/time.rs) | `Stream::throttle`, `Stream::throttle_enforce`, `Stream::debounce` |
| [`group.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/group.rs) | `Stream::group_by_key`, `Stream::group_by` |
| [`broadcast.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/broadcast.rs) | `Stream::broadcast`, `Stream::broadcast_replay`, `broadcast_with_replay` |
| [`transduce.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/transduce.rs) | `state_scan`, `transduce_items` / `via_transducer` |
| [`transducer.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/transducer.rs) | `Transducer`, `transducer_map` / `transducer_filter` / `transducer_take` |

## When to use what

- **Windows** — aggregate or batch elements by count, time, or session gaps.
- **Joins** — correlate two sources (`merge` for fair interleave, `zip` / `zip_with` for positional pairs, `combine_latest` for dashboards, `keyed_join` for per-key correlation).
- **Rate and grouping** — `throttle` / `debounce` on the fiber clock (virtual time under `TestExecutor`), `group_by_key` / `group_by` to split a stream per key with bounded buffers.
- **Replay fanout** — like `broadcast`, plus a retained tail replayed to each branch.
- **`state_scan`** — emit only on FSM transitions (contrast with [`scan`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/streaming/stream.rs), which emits every step).
- **Transducers** — reusable map/filter pipelines (the same type as `id_effect_optics::Transducer`).

See the section pages for examples; the tests in `crates/id_effect/tests/stream*.rs` exercise each operator.
