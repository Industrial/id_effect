# REPORT-0.8: id_effect 0.8 against v0.6 (draft)

Draft. The full same-session comparison on every D9 gate id is written for the 0.8 release; this
file holds the sections recorded by the 0.8 leaves so far.

## E2.1h: the 0.7 queue, channel and retry regressions

Leaf `r08-e2-1h-hygiene` (`.maestro/specs/releases/0.8/e2-1h-hygiene.md`). REPORT-0.7 §4 listed
three report-only regressions against v0.6: `coord/queue_bounded_10k` 2.55×,
`coord/channel_spsc_10k` 1.47× and `phase0/bench_schedule_retry_overhead` 5.77×. They are fixed
here and are now budget gates in `gates.toml`: the two `coord` ids at ≤ 1.25× v0.6, the retry
id at ≤ 2× v0.6.

### Setup

| | |
|---|---|
| Date | 2026-10-16, 13:52 to 14:27 local |
| Machine | AMD Ryzen 9 7950X 16-Core (32 threads), Linux 6.18.55 (as REPORT-0.7) |
| Toolchain | `rustc 1.99.0-nightly (af3d95584 2026-07-09)` |
| Baselines | `v0.6-rerun.json` (`bench/v0.6`, REPORT-0.7 round r2) and `v0.7.json` (0.7.0) |
| 0.8 side | `release/0.7.0` working tree with this leaf (plus an unrelated in-flight change adding relaxed atomic fiber counters to `observability` / `runtime/fiber_context.rs`) |

**Load.** Not a quiet machine: other agents were building in parallel. The 1-minute load average
was between 3.6 and 34 while these numbers were taken (REPORT-0.7 required < 1.5). The `coord`
benches run a producer fiber on a tokio worker and the consumer on the calling thread, so they
are sensitive to load; v0.6 ran both on one thread and is not. Each figure below is therefore
one of several runs, and the gate verdict is given for every run, not for the best one.

### Profile

No sampling profiler is available on this machine (`perf` is not installed, and valgrind is
blocked by the agent shell policy), so the causes were confirmed by code reading, event counters
and A/B runs instead of a flame graph.

- **Queue `offer` / `take`.** Every `offer` was a `from_async` step (a boxed async block around
  an `OfferFuture`, polled through the runloop's future path) even when a slot was free, and
  every `take` was `take_between_effect(1, 1)` (the same, plus a `Vec` for one item and a `map`
  frame to unpack it). A temporary counter build of the bench workload showed the slow paths are
  rare: per 10k items, 0 to 3 offers found the bounded queue full, 235 to 990 takes found it
  empty, and only 6 to 47 takes actually parked. Almost every operation was paying the async
  machinery for nothing.
- **Channel `write` / `read`.** Same pattern: both were `from_async` steps around the queue's
  futures.
- **Contention.** The producer and the consumer run on different threads and each operation
  clones the queue's `Arc`; its counts shared a cache line with the queue's mutex, so the
  count updates and the lock bounced the same line between cores.
- **Retry.** `retry_loop` read the executor clock through `Effect::from_ctx` after every failure.
  That step needs a fiber, so a retry of a synchronous effect could not settle inline in
  `block_on` and paid for a root fiber and a runloop. The attempt was also deferred in its own
  `suspend` with the recovery attached outside it, which erases the attempt into a `Fold` frame
  even when it is a ready `fail`.

### Changes

- `Queue::offer` / `Queue::take` and `Channel::write` / `Channel::read` are one synchronous step
  when they can complete at once (a free slot or waiting taker; an item or a shut-down queue) and
  fall back to the async path only when a bounded queue is full or the queue is empty. `take`
  allocates no `Vec` on either path (its slow path is a single-item `TakeOneFuture`).
- The queue's shared state is 128-byte aligned, so the `Arc` counts are on another cache line
  than the mutex.
- `Schedule` records whether any of its steps reads `now`. The built-in constructors (`recurs`,
  `forever`, `spaced`, `exponential*`, `fibonacci`) do not; `from_fn` and `up_to` do, and the
  combinators propagate it. `retry` / `repeat` read the fiber's clock only for a schedule that
  reads it, and each attempt's recovery is attached to the built attempt, so a ready attempt
  stays a typed synchronous step.

### Results

`bench-compare.sh` medians, three full runs of `gate_core` and `phase0_baseline` (criterion
defaults), against `v0.6-rerun.json` (gate) and `v0.7.json` (0.7.0). The fiber and axum benches
were not re-run, so the comparison uses `gates.toml` restricted to these two benches.

| ID | v0.6 | 0.7.0 | 0.8 run 1 | run 2 | run 3 | ratio vs v0.6 | vs 0.7.0 (run 1) | gate | result |
|---|---|---|---|---|---|---|---|---|---|
| `coord/channel_spsc_10k` | 1.194 ms | 1.750 ms | 870.7 µs | 1.049 ms | 908.5 µs | 0.729 / 0.879 / 0.761 | 0.497 | ≤ 1.25 | **PASS** ×3 |
| `coord/queue_bounded_10k` | 723.2 µs | 1.842 ms | 882.0 µs | 836.0 µs | 888.7 µs | 1.219 / 1.156 / 1.229 | 0.479 | ≤ 1.25 | **PASS** ×3 |
| `phase0/bench_schedule_retry_overhead` | 102.0 ns | 588.4 ns | 136.9 ns | 136.4 ns | 138.9 ns | 1.342 / 1.337 / 1.361 | 0.233 | ≤ 2.0 | **PASS** ×3 |

Load average during the three runs: 3.6 to 10.2, 10.2 to 19.9, 19.9 to 9.2. Every other id in
the two benches stayed within 5 % of 0.7.0 (bind/map budget rows 0.99 to 1.01).

An interleaved A/B series in the same session (each side's criterion run back to back, three
rounds, load 3.5 to 5) gives the same picture against a v0.6 binary run alongside:

| ID | v0.6 (same session) | 0.7.0 HEAD | 0.8 |
|---|---|---|---|
| `coord/channel_spsc_10k` | 1.240 to 1.485 ms | 1.727 ms (one round, load 32) | 894 to 966 µs |
| `coord/queue_bounded_10k` | 769 µs to 1.071 ms | 1.883 ms (one round, load 32) | 835 to 874 µs |
| `phase0/bench_schedule_retry_overhead` | 112 to 114 ns | 635 ns (one round, load 32) | 136 to 142 ns |

### Notes

- `coord/queue_bounded_10k` passes with little margin (1.16 to 1.23 against 1.25). What is
  left is the kernel's per-step cost (three boxes per queue operation: the step, the fused
  `flat_map`, the runloop's erasure) and the cross-thread hand-off, which v0.6 avoided by running
  both sides on one thread. Both are outside this leaf. Re-measure on a quiet machine for the
  release report.
- The retry figure (≈ 137 ns, 1.34× v0.6) is the fiber-free settle path; the remaining cost is
  one `ScheduleState` box per step and the attempt's boxed steps.
