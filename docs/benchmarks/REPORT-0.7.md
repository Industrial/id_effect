# REPORT-0.7: id_effect 0.7.0 against v0.6

Leaf `r-bench-report` (release plan §4 row `r-bench-report`, decision D9, gate H2). This
report compares a same-session re-run of `bench/v0.6` with 0.7.0 on every D9 gate id.

**Result: every gate passes. `bench-compare.sh` exits 0, so no H2 waiver is needed.**

- bind/map hot path (budget ≤ 1.25): it does not regress, it improves (0.41× to 0.87×).
- Fiber spawn, fiber sleep CPU and the axum request path (improve < 0.95): 0.0026×, 0.0019× and
  0.14× v0.6.
- Report-only regressions: `coord/channel_spsc_10k` 1.47×, `coord/queue_bounded_10k` 2.55× and
  `phase0/bench_schedule_retry_overhead` 5.77× (§4).

## 1. Setup

| | |
|---|---|
| Date | 2026-10-15, round r1 16:41 to 16:48 UTC, round r2 16:54 to 17:00 UTC |
| Machine | AMD Ryzen 9 7950X 16-Core (32 threads), 61 GiB RAM, `performance` governor, Linux 6.18.55 |
| Toolchain | `rustc 1.99.0-nightly (af3d95584 2026-07-09)`, `cargo 1.99.0-nightly (59800466c 2026-07-07)` (devenv shell) |
| v0.6 side | `bench/v0.6` = `4894861` (v0.6.0 plus the bench commits), `git archive` export, with the `Cargo.lock` of the earlier v0.6 reruns (b0, k-runloop-opt) |
| 0.7 side | HEAD `47d2f4a` (`release/0.7.0` after r-docs-sweep), `git archive` export, fresh `Cargo.lock` |
| Criterion | default settings (95 % confidence intervals, 100 samples, 10 for the fiber benches) |
| Gates | `gates.toml` as of `bf3d768` (improve limit 0.95 with no noise band, see §5) |

Each side had its own export and its own `CARGO_TARGET_DIR` (`.tmp/target-bre-v06`,
`.tmp/target-bre-head`). Both were built in full (`CARGO_BUILD_JOBS=4`) before any bench ran.
Then each round ran all four gate benches on v0.6 and, straight after, on HEAD.

**Load.** No other build or bench ran on the machine during the rounds (`pgrep -c rustc` was 0
at every 30 s sample). Each round waited until the 1-minute load average was below 1.5. Round r1
started at 1.39, with an unrelated single-threaded Python job still on one core. Round r2
started at 1.32 after that job had ended. A QuestDB JVM (about 45 % of one core) and a desktop
app (about 23 %) ran in the background the whole time. During the fiber and axum benches the load
rose to between 5 and 13. That load came from the benches' own tokio worker threads, mostly on
the v0.6 side, whose fibers spin (see §4).

The committed files come from round r2:

| File | Content |
|---|---|
| `v0.6-rerun.json` | `bench/v0.6` @ `4894861`, criterion baseline `v06-r2` |
| `v0.7.json` | HEAD @ `47d2f4a`, criterion baseline `head-r2` |

Both are plain `bench-export.sh` output. Since `bf3d768` that output also carries criterion's
`mean_ci` and `median_ci` bounds.

## 2. Gate

```text
$ bash scripts/bench-compare.sh docs/benchmarks/v0.6-rerun.json docs/benchmarks/v0.7.json
bench-compare: v06-r2@4894861156a3 -> head-r2@47d2f4aa4cf3 (median)
ID                                     KIND     LIMIT  RATIO   RESULT
phase0/bench_effect_map_chain          budget   1.25   0.796   PASS
phase0/bench_effect_flat_map_chain     budget   1.25   0.617   PASS
gate/flat_map_depth_10k                budget   1.25   0.410   PASS
gate/async_bind_64                     budget   1.25   0.871   PASS
gate/spawn_join_10k                    improve  0.95   0.003   PASS
gate/sleep_cpu_1k_x_1ms                improve  0.95   0.002   PASS
gate/axum_request_oneshot              improve  0.95   0.136   PASS
gate/sleep_1k_x_1ms                    report   -      0.005   REPORT
gate/axum_request_caps                 report   -      0.148   REPORT
stream/pull_map_10k                    report   -      0.553   REPORT
stream/map_effect_1k                   report   -      0.001   REPORT
coord/channel_spsc_10k                 report   -      1.466   REPORT
coord/queue_bounded_10k                report   -      2.547   REPORT
phase0/bench_env_insert_overhead       report   -      0.954   REPORT
phase0/bench_schedule_retry_overhead   report   -      5.766   REPORT
phase0/bench_stream_throughput_collect report   -      0.378   REPORT

bench-compare: 0 gate(s) FAIL
```

Exit code 0.

## 3. Gated ids

The gate metric is the median time per iteration (`sleep_cpu` is process CPU time). Each cell
shows the point estimate and criterion's 95 % confidence interval in brackets. A ratio's interval
is the conservative quotient of the two intervals, `[new.lo / base.hi, new.hi / base.lo]`. The
mean ratio is listed as a cross-check.

| ID | v0.6 median [95 % CI] | 0.7 median [95 % CI] | ratio [CI] | mean ratio [CI] | gate | result |
|---|---|---|---|---|---|---|
| `phase0/bench_effect_map_chain` | 66.69 ns [66.59, 66.75 ns] | 53.10 ns [52.99, 53.25 ns] | 0.796 [0.794, 0.800] | 0.798 [0.796, 0.801] | ≤ 1.25 (budget) | **PASS** |
| `phase0/bench_effect_flat_map_chain` | 89.87 ns [89.75, 89.99 ns] | 55.42 ns [55.19, 55.57 ns] | 0.617 [0.613, 0.619] | 0.609 [0.604, 0.615] | ≤ 1.25 (budget) | **PASS** |
| `gate/flat_map_depth_10k` | 481.9 µs [481.7, 483.1 µs] | 197.6 µs [196.0, 198.1 µs] | 0.410 [0.406, 0.411] | 0.408 [0.406, 0.409] | ≤ 1.25 (budget) | **PASS** |
| `gate/async_bind_64` | 1.610 µs [1.604, 1.625 µs] | 1.402 µs [1.398, 1.413 µs] | 0.871 [0.860, 0.881] | 0.869 [0.860, 0.878] | ≤ 1.25 (budget) | **PASS** |
| `gate/spawn_join_10k` | 6.670 s [6.657, 6.776 s] | 17.20 ms [13.91, 18.25 ms] | 0.00258 [0.00205, 0.00274] | 0.00244 [0.00212, 0.00272] | < 0.95 (improve) | **PASS** |
| `gate/sleep_cpu_1k_x_1ms` | 6.767 s cpu [6.705, 6.844 s] | 12.66 ms cpu [12.53, 12.73 ms] | 0.00187 [0.00183, 0.00190] | 0.00186 [0.00184, 0.00188] | < 0.95 (improve) | **PASS** |
| `gate/axum_request_oneshot` | 52.50 µs [52.18, 52.87 µs] | 7.157 µs [7.030, 7.364 µs] | 0.136 [0.133, 0.141] | 0.148 [0.143, 0.155] | < 0.95 (improve) | **PASS** |

Even the upper end of every ratio interval clears its limit by a wide margin. The tightest
budget row is `async_bind_64` at 0.881 against 1.25. The tightest improve row is
`axum_request_oneshot` at 0.141 against 0.95.

**Round r1** (same session, same builds) gives the same verdict, with 0 FAIL and exit 0:

| ID | r1 ratio | r2 ratio |
|---|---|---|
| `phase0/bench_effect_map_chain` | 0.796 | 0.796 |
| `phase0/bench_effect_flat_map_chain` | 0.599 | 0.617 |
| `gate/flat_map_depth_10k` | 0.399 | 0.410 |
| `gate/async_bind_64` | 0.853 | 0.871 |
| `gate/spawn_join_10k` | 0.00306 | 0.00258 |
| `gate/sleep_cpu_1k_x_1ms` | 0.00186 | 0.00187 |
| `gate/axum_request_oneshot` | 0.139 | 0.136 |

Between rounds the v0.6 side moved by at most 3.5 % on any id. The 0.7 side moved by about
5 % at most, except `spawn_join_10k`, which moved 23 % (21.1 ms against 17.2 ms). That bench is noisy
with only 10 samples, as SPIKE-REPORT §2 already noted for both spikes. Its ratio stays below
0.004 in both rounds. The v0.6 rerun also matches the W0 baseline `v0.6.json` within 6.5 % on
every gated id (largest gap: `axum_request_oneshot`, rerun at 0.935× the W0 figure), so the old
side still measures what b0 captured.

## 4. Other ids (report only)

| ID | v0.6 median [95 % CI] | 0.7 median [95 % CI] | ratio [CI] | mean ratio [CI] |
|---|---|---|---|---|
| `gate/sleep_1k_x_1ms` | 689.0 ms [683.3, 696.4 ms] | 3.350 ms [3.329, 3.386 ms] | 0.00486 [0.00478, 0.00496] | 0.00486 [0.00480, 0.00492] |
| `gate/axum_request_caps` | 53.89 µs [53.71, 54.21 µs] | 7.963 µs [7.942, 8.010 µs] | 0.148 [0.147, 0.149] | 0.147 [0.145, 0.150] |
| `stream/pull_map_10k` | 17.19 µs [17.12, 17.22 µs] | 9.514 µs [9.499, 9.526 µs] | 0.553 [0.552, 0.556] | 0.556 [0.553, 0.559] |
| `stream/map_effect_1k` | 44.98 ms [44.88, 45.04 ms] | 44.70 µs [44.65, 44.75 µs] | 0.00099 [0.00099, 0.00100] | 0.00099 [0.00099, 0.00100] |
| `coord/channel_spsc_10k` | 1.194 ms [1.193, 1.196 ms] | 1.750 ms [1.704, 1.757 ms] | 1.466 [1.425, 1.473] | 1.446 [1.428, 1.465] |
| `coord/queue_bounded_10k` | 723.2 µs [722.6, 725.1 µs] | 1.842 ms [1.834, 1.850 ms] | 2.547 [2.530, 2.561] | 2.577 [2.541, 2.618] |
| `phase0/bench_env_insert_overhead` | 36.69 ns [36.25, 36.91 ns] | 35.00 ns [33.91, 35.25 ns] | 0.954 [0.919, 0.973] | 0.936 [0.926, 0.946] |
| `phase0/bench_schedule_retry_overhead` | 102.0 ns [101.9, 102.3 ns] | 588.4 ns [587.9, 588.8 ns] | 5.766 [5.746, 5.781] | 5.749 [5.717, 5.773] |
| `phase0/bench_stream_throughput_collect` | 3.707 µs [3.701, 3.717 µs] | 1.403 µs [1.400, 1.405 µs] | 0.378 [0.377, 0.380] | 0.377 [0.376, 0.379] |

In r1 these were 1.535 (`channel_spsc`), 2.506 (`queue_bounded`) and 5.997
(`schedule_retry`). Every other id was within 5 % of its r2 ratio.

## 5. Discussion

- **bind/map.** D9 allowed the hot path to regress by up to 25 %. Instead every row got faster:
  `map_chain` 0.80×, `flat_map_chain` 0.62×, `flat_map_depth_10k` 0.41× and `async_bind_64`
  0.87×. These match the kernel-only `k-runloop-opt` numbers (0.80, 0.60, 0.40, 0.83) now that
  the former `next` kernel is the crate root. Retiring v1 changed nothing on this path.
- **Fibers.** v0.6 drove fibers through `run_blocking` plus a `yield_now` spin (SPIKE-REPORT §2).
  That is why 10k spawn/join took 6.7 s and 1k × 1 ms sleeps burnt 6.8 s of CPU. 0.7 fibers use
  real wakers on the tokio runtime, which is 390× faster for spawn/join (17 ms) and 535× less
  CPU for sleep (12.7 ms). Wall time for the sleeps falls from 689 ms to 3.35 ms. `stream/map_effect_1k`
  (45 ms → 45 µs, an effect per element) most likely gains for the same reason; that was not
  profiled.
- **axum.** Each request's effect now runs as a fiber on the serving runtime, with no hop to a
  dedicated executor thread. That cuts `axum_request_oneshot` from 52.5 µs to 7.2 µs (0.14×), and
  the capability overlay path improves the same way (0.15×).
- **Report-only regressions (not gated by D9, no waiver needed):**
  - `coord/queue_bounded_10k` 2.55× and `coord/channel_spsc_10k` 1.47×, against the v0.6 coordination
    primitives. The cause was not investigated here. These are the first candidates if a 0.7.x
    performance leaf is opened.
  - `phase0/bench_schedule_retry_overhead` rose from 102 ns to 588 ns for three attempts. That is
    about 160 ns more per attempt. The 0.7 bench runs `Effect::retry` through `block_on` and
    the 0.7 schedule driver, where 0.6 ran a `pollster` loop over a boxed future. Which part
    costs the time was not profiled. Retry is not a hot path, but the figure should be tracked.
- **Gate change (orchestrator ruling).** Before `bf3d768`, an `improve` gate had limit 1.00
  plus the global 0.05 noise band, so it only failed at a ratio of 1.05 or more. A result
  slightly slower than v0.6 met "must improve". `gates.toml` now gives the three improve gates
  `limit = 0.95` and `noise = 0`, and `bench-compare.sh` accepts a per-gate `noise`. The budget
  gates are unchanged (≤ 1.25 plus 0.05 noise). With this rule a same-build comparison FAILs
  the improve gates, so the self-test's identity case now expects exactly those three failures.
  Release-plan row `b0-bench-tooling` still says "< 1.00" and should be read with this ruling.
  It applies to this report's numbers either way: the largest improve ratio is 0.141.
- **Caveats.** These are two rounds on one machine, not an interleaved multi-round median (unlike
  `k-runloop-opt.md`). The margins are large enough that this does not affect any verdict.
  Expect codegen sensitivity of about ±20 % on `async_bind_64` across builds (SPIKE-REPORT §2).
  The bench sources differ between the two trees only by the API port (`k-retire`); the ids and
  workloads are the same.

## H2

Not needed: `bash scripts/bench-compare.sh docs/benchmarks/v0.6-rerun.json docs/benchmarks/v0.7.json`
exits 0 with 0 FAIL, and no gate is waived.

## 6. Reproduce

```bash
# Exports (no worktree; the shared checkout stays on its branch), one target dir per export.
git archive bench/v0.6 | tar -x -C .tmp/bre-v06 && git rev-parse bench/v0.6 > .tmp/bre-v06/.bench-sha
cp .tmp/bench-v0.6/Cargo.lock .tmp/bre-v06/              # the lock the earlier v0.6 reruns used
git archive HEAD | tar -x -C .tmp/bre-head && git rev-parse HEAD > .tmp/bre-head/.bench-sha

# Build both fully first (CARGO_BUILD_JOBS=4, `cargo bench --no-run` per gate bench), then on a
# quiet machine run v0.6 and HEAD back to back:
devenv shell -- bash scripts/bench-run.sh --src .tmp/bre-v06  --baseline v06-r2  --target-dir .tmp/target-bre-v06
devenv shell -- bash scripts/bench-run.sh --src .tmp/bre-head --baseline head-r2 --target-dir .tmp/target-bre-head
devenv shell -- bash scripts/bench-export.sh --target-dir .tmp/target-bre-v06  v06-r2  docs/benchmarks/v0.6-rerun.json
devenv shell -- bash scripts/bench-export.sh --target-dir .tmp/target-bre-head head-r2 docs/benchmarks/v0.7.json
bash scripts/bench-compare.sh docs/benchmarks/v0.6-rerun.json docs/benchmarks/v0.7.json
```

The CI columns in §3 and §4 come from the `median_ci` and `mean_ci` fields of the two JSON files.
