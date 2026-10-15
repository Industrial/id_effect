# k-runloop-opt: the `next` runloop against v0.6

Leaf `k-runloop-opt` (spec `k-rebuild` §3.11, H1 condition 1): bring `gate/async_bind_64` of the
0.7 kernel (`id_effect::next`) to ≤ 1.25× v0.6 without regressing the sync rows or the runloop's
semantics.

**Result: target met.** `gate/async_bind_64` is at **0.833×** v0.6 (was 2.714×), every
`gates-k.toml` row passes, and no semantics changed. H2 is not needed for this gate.

Commits: `e5121b0` (local `~` bind mask fix), `af0fa1a` (runloop), plus this report and its
exports.

## 1. Gate (3 interleaved rounds, median of round medians)

```text
$ bash scripts/bench-compare.sh --gates docs/benchmarks/gates-k.toml \
    docs/benchmarks/v0.6-k-rerun.json docs/benchmarks/k-post-opt.json
bench-compare: v0.6-k-rerun@4894861156a3 -> k-post-opt@af0fa1a46c31 (median)
ID                                     KIND     LIMIT  RATIO   RESULT
phase0/bench_effect_map_chain          budget   1.25   0.801   PASS
phase0/bench_effect_flat_map_chain     budget   1.25   0.603   PASS
gate/flat_map_depth_10k                budget   1.25   0.401   PASS
gate/async_bind_64                     budget   1.25   0.833   PASS
coord/channel_spsc_10k                 report   -      1.011   REPORT
coord/queue_bounded_10k                report   -      0.994   REPORT
phase0/bench_env_insert_overhead       report   -      0.959   REPORT
phase0/bench_schedule_retry_overhead   report   -      1.015   REPORT
phase0/bench_stream_throughput_collect report   -      1.028   REPORT
stream/map_effect_1k                   report   -      1.000   REPORT
stream/pull_map_10k                    report   -      1.043   REPORT

bench-compare: 0 gate(s) FAIL
```

Before the leaf (`k-pre-opt.json`, HEAD `8864d96`, the k-benches-next AC5 measurement), against
the same v0.6 rerun:

```text
bench-compare: v0.6-k-rerun@4894861156a3 -> k-pre-opt@8864d96a239c (median)
phase0/bench_effect_map_chain          budget   1.25   0.792   PASS
phase0/bench_effect_flat_map_chain     budget   1.25   0.605   PASS
gate/flat_map_depth_10k                budget   1.25   0.629   PASS
gate/async_bind_64                     budget   1.25   2.714   FAIL
bench-compare: 1 gate(s) FAIL
```

| ID | v0.6 (rerun) | pre-opt | post-opt | pre / v0.6 | post / v0.6 |
|---|---|---|---|---|---|
| `gate/async_bind_64` | 1.639 µs | 4.449 µs | 1.365 µs | 2.714 | **0.833** |
| `phase0/bench_effect_map_chain` | 66.8 ns | 52.9 ns | 53.5 ns | 0.792 | 0.801 |
| `phase0/bench_effect_flat_map_chain` | 90.5 ns | 54.8 ns | 54.6 ns | 0.605 | 0.603 |
| `gate/flat_map_depth_10k` | 487.2 µs | 306.3 µs | 195.3 µs | 0.629 | 0.401 |

Round medians, `gate/async_bind_64`: post 1363 / 1365 / 1369 ns, pre 4383 / 4451 / 4449 ns,
v0.6 1604 / 1611 / 1639 (pre session) and 1666 / 1702 / 1639 ns (post session).

### Files and rounds

| File | Content |
|---|---|
| `k-pre-opt.json` | HEAD `8864d96` (before this leaf), 3 rounds |
| `k-post-opt.json` | HEAD `af0fa1a` (this leaf), 3 rounds |
| `v0.6-k-rerun.json` | `bench/v0.6` (`4894861`), the 6 v0.6 rounds interleaved with the two sessions above |

Each file is in the `bench-export.sh` format; the `rounds` object lists the criterion baselines,
the per-round medians and the quiet notes. Each round ran v0.6, then HEAD, with default criterion
settings, `--only id_effect:gate_core --only id_effect:phase0_baseline`, on the `performance`
governor (CPU and `rustc -V` in `machine`). Before each side the driver waited until
`pgrep -c rustc` was 0 (polling every 60 s). Quiet record:

- Pre session (03:15 to 03:28 UTC): all six sides quiet at start.
- Post session r1 to r3 (04:15 to 04:30 UTC): all sides quiet at start; on r3 another agent's
  `rustc` appeared while the v0.6 side ran (contended).
- Two extra post rounds with a 5 s `rustc` sampler during the run: r4 v0.6 contended (10
  samples), r4 HEAD quiet; r5 v0.6 lightly contended (3 samples), r5 HEAD quiet. They are not in
  the committed files. With all five post rounds the ratio is 0.824; against the post session's
  own three v0.6 rounds it is 0.819; against the committed `v0.6.json` (b0 baseline) 0.861.
  The contended v0.6 rounds (r3 1639 ns, r4 1662 ns, r5 1657 ns) sit inside the spread of the
  quiet ones (1604 to 1702 ns), and the HEAD side varies by 1.3 % over all five rounds
  (1363 to 1380 ns).

## 2. Where the time went

The spike report blamed per-step `catch_unwind`, three interrupt-flag loads per bind and
`AnyBox` erasure. Measured with `perf annotate` and `callgrind`, those were small. The cost was:

1. **Store-to-load forwarding stalls.** Every step moved large enums (`Instr` 40 B, `Effect`
   48 B, `AnyExit` 40 B) through memory right after the callee wrote them: the callee stores
   field by field (8 B), the caller copies with 16 B loads, and a load that spans two smaller
   in-flight stores cannot be forwarded. The hot instructions were plain `movups` copies after
   `k(v)` returned, after a future's `poll` returned, and in the builder's
   `effect = effect.flat_map(..)` (copying the `Seq`'s `Vec` out and back). With the allocator
   fixed (row 4 below), the async loop still ran at about 1.6 instructions per cycle.
2. **The allocator's slow path.** `FUSE_MAX = 64` built one boxed closure per bind, all live at
   once. glibc's thread cache holds 7 chunks per size class, so most of those went through
   `_int_malloc`/`_int_free`, and the 1.5 KB frame-stack growth triggered
   `malloc_consolidate` every run (about half of all instructions in `callgrind`).

## 3. Ablation

Probe: a release binary that runs the `gate/async_bind_64` body at n binds through
`Effect::block_on` (best of 7 timed loops), the `map_chain` and `flat_map_depth_10k` bodies, and a
counting global allocator in a separate build. Same machine, rustc-quiet, desktop load in the
background; one row per change, in the order applied. n1 is the edge cost, marginal is the slope
from n16 to n256.

| # | Change | n1 | n16 | n64 | n256 | marginal | map_chain | depth 10k |
|---|---|---|---|---|---|---|---|---|
| 0 | base `8864d96` | 144 | 1117 | 4594 | 14484 | 55.7 | 48 | 302 µs |
| 1 | one `catch_unwind` per poll (was per step) | 144 | 1143 | 4715 | 14957 | 57.6 | 47 | 349 µs |
| 2 | `Seq` holds `Frame`s, empty stack adopts it (+ thread-local frame cache, dropped in 8) | 142 | 1109 | 4232 | 15324 | 59.2 | 48 | 360 µs |
| 3 | `drive`: instruction in a local, fresh future polled at once | 141 | 1037 | 3907 | 14082 | 54.4 | 47 | 340 µs |
| 4 | `FUSE_MAX` 64 → 4, fiber-free `settle_instr`, shared `HNil` env | 152 | 982 | 3357 | 12880 | 49.6 | 47 | 308 µs |
| 5 | `#[inline(always)]` `into_instr`, `seq` | 157 | 910 | 3101 | 12108 | 46.7 | 52 | 277 µs |
| 6 | `unwind` returns the `Instr` unwrapped (no `Result`) | 163 | 858 | 2924 | 11153 | 42.9 | 52 | 277 µs |
| 7 | bind fast path `Runloop::feed` | 155 | 638 | 1883 | 6970 | 26.4 | 53 | 275 µs |
| 8 | drop both thread-locals (frame cache, parking waker) | 157 | 658 | 1969 | 7119 | 26.9 | 53 | 278 µs |
| 9 | `Node::Sync` depth `u16` → `usize` (aligned layout) | 155 | 638 | 1916 | 6972 | 26.4 | 52 | 275 µs |
| 10 | `feed` rebuilds the carried value by variant | 153 | 611 | 1826 | 6620 | 25.0 | 52 | 276 µs |
| 11 | `Instr::Seq(Box<SeqBody>)`: inner and frames boxed as one unit | 148 | 536 | 1372 | 4673 | 17.2 | 52 | 195 µs |

All times in ns unless marked. Row 1 is within noise (it removes no measurable cost, but it is
what lets 7 run a chain of binds without a boundary per step). Row 2 helps n64 and hurts n256
(one spare buffer for two `Seq` lists); its cache part was dropped in row 8. The large steps are
7 and 11, both copy eliminations.

The committed criterion probes (`next_kernel` bench, default settings, run once after each
session) agree:

| `next_probe/async_bind_n*` | n1 | n16 | n64 | n256 | marginal |
|---|---|---|---|---|---|
| pre (`8864d96`) | 141.1 ns | 1.075 µs | 4.174 µs | 13.92 µs | 53.5 ns |
| post (`af0fa1a`) | 147.6 ns | 526.3 ns | 1.375 µs | 4.775 µs | 17.7 ns |

The marginal cost per bind fell by 67 % (v0.6: 34.3 ns, SPIKE-REPORT §3), below the 43 ns the
spec asked for. The edge (n1) is 6.5 ns (5 %) slower, from the fiber-free prefix that `block_on`
now runs first; that is 0.5 % of the gate figure.

**Allocations** (counting allocator, one call): n64 135 allocations + 5 reallocations before,
79 + 3 after. Per bind (slope n16 to n256): 1.21 → 1.00 allocations and 92.6 → 56 bytes (v0.6:
3.0 and 80). The one allocation left per bind is the `from_async` future box.

### Measured and not kept

- **Thread-local caches.** A per-thread spare frame buffer (−3 to −5 % at n64) and a per-thread
  parking waker for `block_on` (−3 % at n1). `tests/scope.rs`
  `next_has_no_thread_locals` (k-scope-fiberref AC5) forbids `thread_local!` under `src/next`,
  so both were removed (row 8). They are options for `r-bench-report` if that rule is relaxed
  for non-fiber state.
- **No fusion at all** (every `map`/`flat_map` a `Seq` frame, run fiber-free): `map_chain`
  rose from 48 ns to 170 ns, because erased steps pay the copies described in section 2.
  `FUSE_MAX = 1` sends the sync rows through the fiber edge (268 ns). Kept: `FUSE_MAX = 4`, so
  short ready chains stay one typed closure and long chains never hold more than 4 live boxes.
- **`#[inline]` on `map`/`flat_map`**: no change, reverted.
- **Amortised interrupt checks**: not needed. A check is one load from the fiber; after row 7
  there are two per bind (before the continuation, before the first poll), as before.

## 4. Semantics

Unchanged, and tested (in debug and `--release`):

- **Panic → `Die` with payload, finalizers run.** `catch_unwind` is now once per
  `Runloop::poll` (and once per `settle_instr` call). A panic leaves every pending frame on the
  stack; the loop resumes with `Die` and unwinds as for any failure. Tests: a panicking step
  after a ready value and after an async step, both under a `Fold` that recovers (the handler
  ran once, the payload is the original `String`, the loop goes on), in the runloop and in
  `settle_instr`; a panicking `poll`; `next_kernel`
  `a_panic_in_a_sync_step_is_die_after_the_finalizer_ran` (after 0, 3, 10 and 1000 binds, with
  `ensuring`, through `run_on` and `block_on`).
- **Interrupt latency.** Still observed before every continuation and before the first poll of
  every future, and while suspended through the waker. Tests: an interrupt requested by step
  5000 of 10 000 fast-path binds stops the chain at exactly 5000; the same through 2000
  `from_async` binds in `next_kernel`; the existing mask-region and budget tests.
- **Stack safety at 100k**, laziness, masks and finalizers: the existing `next_kernel`,
  `next_fibers`, `next_scope`, `next_edge`, `next_typed_r`, `next_layer_base` and `next_macros`
  suites (139 tests) pass unchanged.
- **`~` binds in `effect_local_next!`** now hold a mask level while suspended, as the `Send`
  bind does (commit `e5121b0`). Before, an interrupt of a suspended local bind dropped the
  nested runloop without running the sub-effect's finalizers. Two unit tests in
  `kernel/macro_support.rs` fail without the fix.

No `unsafe`, no new dependency, no public signature change, v1 untouched.

## 5. Reproduce

```bash
bash scripts/bench-run.sh --src .tmp/bench-v0.6 --baseline v06-rN --only id_effect:gate_core --only id_effect:phase0_baseline
bash scripts/bench-run.sh --src <export of HEAD> --baseline k-rN --only id_effect:gate_core --only id_effect:phase0_baseline
bash scripts/bench-export.sh v06-rN v06-rN.json   # and k-rN
# median of round medians per id -> the committed files; then:
bash scripts/bench-compare.sh --gates docs/benchmarks/gates-k.toml \
    docs/benchmarks/v0.6-k-rerun.json docs/benchmarks/k-post-opt.json
cargo bench -p id_effect --bench next_kernel   # next_probe/async_bind_n{1,16,64,256}
```
