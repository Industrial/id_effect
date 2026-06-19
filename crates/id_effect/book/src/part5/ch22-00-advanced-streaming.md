# Advanced Streaming

**Part V · Chapter 22** — windowing, joins, replay fanout, FSM scans, and transducers.

Part IV introduced pull-based [`Stream`](../../src/streaming/stream.rs) processing with chunks, sinks, and backpressure. This chapter covers **multi-stream** patterns: grouping events into windows, joining live sources, replaying history to fanout branches, stepping simple state machines over elements, and applying composable transducers.

## Module map

| Module | Role |
|--------|------|
| [`window`](../../src/streaming/window.rs) | Tumbling, sliding, and session windows |
| [`join`](../../src/streaming/join.rs) | `merge`, `combine_latest`, `keyed_join` |
| [`replay`](../../src/streaming/replay.rs) | `broadcast_with_replay` fanout |
| [`state_scan`](../../src/streaming/state_scan.rs) | Optional-output FSM step |
| [`transducer`](../../src/streaming/transducer.rs) | `via_transducer` / `transduce_items` |

## When to use what

- **Windows** — aggregate or batch events by count, time, or session gaps.
- **Joins** — correlate two live sources (`merge` for fair interleave, `combine_latest` for dashboards).
- **Replay fanout** — same as `broadcast`, plus a retained tail buffer per branch.
- **`state_scan`** — emit only on FSM transitions (contrast with [`scan`](../../src/streaming/stream.rs) which emits every step).
- **Transducers** — reusable map/filter pipelines (API-compatible with `id_effect_optics::Transducer`).

See the section pages for examples and tests in each module.
