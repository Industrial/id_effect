# Benchmarks

Performance gates for 0.7 against the v0.6.0 baseline (decision D9 in
`.maestro/specs/0.7/brief.md`). Criterion keeps its baselines in `target/`, so the
numbers that gates read are exported to JSON and committed here.

| File | Content |
|---|---|
| `gates.toml` | The D9 gates read by `scripts/bench-compare.sh` |
| `v0.6.json`, `v0.6.machine.txt` | The v0.6 baseline and the machine it ran on (`b0-baseline-capture`) |
| `v0.6-rerun.json`, `v0.7.json`, [`REPORT-0.7.md`](REPORT-0.7.md) | The 0.7.0 release comparison: a same-session re-run of `bench/v0.6` against 0.7.0 (`r-bench-report`) |
| `fixtures/*.json` | Synthetic exports for `bench-compare.sh --self-test` |
| `compile-0.8.json` | Build loop of the template app (`examples/cli-minimal`) for 0.8: cold build, incremental rebuild, boot ([Compile time](#compile-time)) |

## Gates

`ratio = new / base` on the median time per iteration, so lower is better.

| Kind | Benches | Passes when |
|---|---|---|
| `budget` | `phase0/bench_effect_map_chain`, `phase0/bench_effect_flat_map_chain`, `gate/flat_map_depth_10k`, `gate/async_bind_64` | ratio ≤ 1.25 |
| `improve` | `gate/spawn_join_10k`, `gate/sleep_cpu_1k_x_1ms`, `gate/axum_request_oneshot` | ratio < 0.95 (no noise band) |
| `report` | every other listed or exported bench | always (ratio is printed) |

- **Noise (Q7):** a `budget` ratio that misses its limit by at most 0.05 is reported
  as `NOISE` and does not fail. A gate can set its own `noise`; the `improve` gates set
  `noise = 0` with a limit of 0.95, so "must improve" is not met by a result within
  noise of v0.6 (r-bench-report ruling). A same-build comparison (ratio 1.00)
  therefore FAILs the three `improve` gates and passes the rest.
- **Presence:** every id in `gates.toml`, `report` ones included, must exist in both
  files. A missing id is a `FAIL`.
- **Exit code:** `bench-compare.sh` exits 1 on any `FAIL` and 2 on misuse.
- **Waivers:** a remaining violation after the one optimisation leaf Q7 allows needs
  an explicit human waiver (H2), recorded in `REPORT-0.7.md`. The gate itself is
  not relaxed.

## Benches

| Package | Bench | Ids |
|---|---|---|
| `id_effect` | `phase0_baseline` | `phase0/*` |
| `id_effect` | `gate_core` | `gate/async_bind_64`, `gate/flat_map_depth_10k`, `stream/*`, `coord/*` |
| `id_effect_tokio` | `gate_fiber` | `gate/spawn_join_10k`, `gate/sleep_1k_x_1ms`, `gate/sleep_cpu_1k_x_1ms` (process CPU time) |
| `id_effect_axum` | `gate_request` | `gate/axum_request_oneshot`, `gate/axum_request_caps` |

## Workflow

Run everything through `devenv shell -- …` on the reference machine, with the
`performance` CPU governor, and run both sides in the same session.

```bash
# 1. The v0.6 baseline: bench/v0.6 is exported with `git archive` into .tmp/bench-v0.6
#    (no worktree, the shared checkout stays on its branch) and saved as baseline v0.6.
bash scripts/bench-run.sh --ref bench/v0.6
bash scripts/bench-export.sh --machine-txt docs/benchmarks/v0.6.machine.txt \
    v0.6 docs/benchmarks/v0.6.json

# 2. The current checkout, saved as baseline head into the same target/criterion.
bash scripts/bench-run.sh
bash scripts/bench-export.sh head .tmp/bench-head.json

# 3. Apply the gates.
bash scripts/bench-compare.sh docs/benchmarks/v0.6.json .tmp/bench-head.json
```

- `bench-run.sh --src DIR` benches an existing export instead, for example
  `--src .tmp/bench-v0.6 --baseline v0.6` to re-run it without re-exporting. The SHA
  recorded at export time (`.bench-sha`) is kept.
- `--only PKG:BENCH` limits the run; arguments after `--` go to criterion, for
  example `-- --sample-size 10` for a quick smoke run.
- `bench-run.sh` records the benched SHA in `target/criterion/.bench-run/<baseline>.sha`,
  which `bench-export.sh` puts in the JSON with the CPU, governor, `rustc -V` and kernel.
- Do not name a baseline `base` or `new`; criterion uses those directories itself.

## Self-test

```bash
bash scripts/bench-compare.sh --self-test
```

It compares `fixtures/base.json` with itself (fails on exactly the three `improve` gates)
and with `pass.json` (passes, with a `budget` `NOISE` row), and with `fail-budget.json`, `fail-improve.json` and
`fail-missing.json`. Each of those must fail on exactly its one broken gate.

## Compile time

`scripts/compile-time.sh` measures the build loop of the template app
(`examples/cli-minimal`); `compile-0.8.json` is the 0.8 figure with its machine. It is
not part of `ci` (`moon run effect:compile-time`, `runInCI: false`).

```bash
bash scripts/compile-time.sh --runs 5          # one text line: medians and every sample
bash scripts/compile-time.sh --runs 5 --json   # {crate, cold_s, incremental_s, boot_s, runs, rustc, wrapper}
bash scripts/compile-time.sh --self-test       # no build: median, output format, manifest copy, misuse
```

- **Copy, not the checkout.** The crate is copied to `.tmp/compile-time/` the way a user
  starts from the template: `path = "../…"` dependencies become absolute, `.workspace = true`
  package keys take the root's values, and the copy gets its own `[workspace]` and the
  root `Cargo.lock`. Repository sources are never edited.
- **Cold:** an empty target dir, then `cargo build` (dev profile). Dependency sources are
  fetched once beforehand, untimed, so the figure is compile time, not network time.
- **Incremental:** one appended line
  (`#[allow(dead_code)] const COMPILE_TIME_PROBE: u32 = <run>;`) in `src/lib.rs`, then
  `cargo build` again (lib and binary rebuild and relink).
- **Boot:** one run of `target/debug/cli_minimal --token compile-time` to its exit.
- **Toolchain as shipped:** `RUSTC_WRAPPER`/`RUSTC_WORKSPACE_WRAPPER` (sccache in devenv)
  and `CARGO_INCREMENTAL` are unset; the linker is the toolchain default (no mold).
- **Median of N** (default 3) runs; each run repeats all three steps. The load average
  goes to stderr. Run on an idle machine and compare figures only within one session.

The scorecard reads `dx.compile` #2 (cold < 60 s) from `cold_s`, #1 (rebuild+restart
< 2 s) from `incremental_s + boot_s`, and #6 (dev boot < 5 s) from `boot_s`, medians only.
The 0.8 run was taken on a loaded machine (1-min load 9–18 on 32 threads): cold 10.16 s,
incremental 0.88 s, boot 0.004 s. Two of the five incremental samples were 2.8–3.0 s, so
re-measure #1 on an idle machine before relying on its margin.

## Spike gate (historical)

`SPIKE-REPORT.md` and `spike-gate/` were measured on the two kernel spikes under `spikes/`,
which `k-ui-suite` deleted once design A landed in `id_effect::next`. To reproduce them,
export the pinned spike tree (`git archive 9cf380e | tar -x -C .tmp/spike-9cf380e`) and run
the spike's own bench instructions from `spikes/README.md` in that export. The kernel gates
are now `gates-k.toml`, measured on `id_effect::next` (`k-runloop-opt.md`).
