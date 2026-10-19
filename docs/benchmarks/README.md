# Benchmarks

Performance gates for 0.7 against the v0.6.0 baseline (decision D9 in
`.maestro/specs/0.7/brief.md`). Criterion keeps its baselines in `target/`, so the
numbers that gates read are exported to JSON and committed here.

| File | Content |
|---|---|
| `gates.toml` | The D9 gates read by `scripts/bench-compare.sh` |
| `v0.6.json`, `v0.6.machine.txt` | The v0.6 baseline and the machine it ran on (`b0-baseline-capture`) |
| `v0.6-rerun.json`, `v0.7.json`, [`REPORT-0.7.md`](REPORT-0.7.md) | The 0.7.0 release comparison: a same-session re-run of `bench/v0.6` against 0.7.0 (`r-bench-report`) |
| `v0.9.json`, `v0.10.json`, `v0.10-fiber-registry.json` | The 0.10.0 exit gate: `v0.9.0` (a `git archive` export) and 0.10.0 with the `fiber-registry` feature off and on, one session ([0.10 exit](#010-exit-r10-w3-kpis)) |
| `fixtures/*.json` | Synthetic exports for `bench-compare.sh --self-test` |
| `compile-0.8.json` | Build loop of the template app (`examples/cli-minimal`) for 0.8: cold build, incremental rebuild, boot ([Compile time](#compile-time)) |
| `dx-kpis-0.8.json`, `dx-kpis-0.9.json`, `dx-kpis-0.10.json` | The DX KPIs of PLAN-OF-PLANS §8 measured by `scripts/dx-kpis.sh` (K1, K4, K5; K3 from 0.9; K2 and K6 from 0.10, [Rebuild and release build](#rebuild-and-release-build-k2-k6)) |
| `k3/invoice.patch` | The K3 "invoice" resource for the Ledger reference app, with its reminder job from 0.10 ([Feature LOC (K3)](#feature-loc-k3)) |

## Gates

`ratio = new / base` on the median time per iteration, so lower is better.

| Kind | Benches | Passes when |
|---|---|---|
| `budget` | `phase0/bench_effect_map_chain`, `phase0/bench_effect_flat_map_chain`, `gate/flat_map_depth_10k`, `gate/async_bind_64` | ratio ≤ 1.25 |
| `improve` | `gate/spawn_join_10k`, `gate/sleep_cpu_1k_x_1ms`, `gate/axum_request_oneshot` | ratio < 0.95 (no noise band) |
| `delta` | `gate/axum_request_secure_stack` over `gate/axum_request_bare` | new − new ≤ 5000 ns (no noise band; [Secure-stack budget](#secure-stack-budget-011)) |
| `report` | every other listed or exported bench | always (ratio is printed) |

- **Noise (Q7):** a `budget` ratio that misses its limit by at most 0.05 is reported
  as `NOISE` and does not fail. A gate can set its own `noise`; the `improve` gates set
  `noise = 0` with a limit of 0.95, so "must improve" is not met by a result within
  noise of v0.6 (r-bench-report ruling). A same-build comparison (ratio 1.00)
  therefore FAILs the three `improve` gates and passes the rest.
- **Presence:** every id in `gates.toml`, `report` ones included, must exist in both
  files (a `pair` or `delta` gate's two ids in the new one only). A missing id is a `FAIL`.
- **Pair gates** (`kind = "pair"`, `over = "<id>"`) are a ratio inside one run:
  `new[id] / new[over]`. The base file is not read for them, so a pair added in a change
  is gated in that change already. No shipped gate uses `pair` since 0.11; the self-test
  keeps it covered with `fixtures/gates-pair.toml`.
- **Delta gates** (`kind = "delta"`, `over = "<id>"`) are an absolute difference inside one
  run: `new[id] - new[over]` in the metric's unit (ns), against `limit` in ns. The `RATIO`
  column then shows the difference (`+4650`), and the result the ratio for information
  (`PASS (ratio 6.17)`). The global 0.05 band does not apply; only a `noise` set on the gate
  (in ns) does.
- **`--enforce KINDS`** (comma list of `budget`, `improve`, `pair`, `delta`): only those
  kinds' missed limits `FAIL`; the others print `IGNORED`. A missing id still `FAIL`s. The CI
  job uses `--enforce budget,delta`.
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
| `id_effect_axum` | `gate_request` | `gate/axum_request_oneshot`, `gate/axum_request_caps`, `gate/axum_request_bare`, `gate/axum_request_secure_stack` |

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
and with `pass.json` (passes, with a `budget` `NOISE` row and the secure stack +2400 ns over
`bare`), and with `fail-budget.json`, `fail-improve.json`, `fail-missing.json`,
`fail-delta.json` (+5001 ns, 1 ns over the budget) and `fail-pair-missing.json` (no `bare`).
Each of those must fail on exactly its one broken gate. The `ci-*` cases repeat them with
`--enforce budget,delta`: the identity comparison then passes with the three `improve` gates
`IGNORED`, the budget, delta and missing-id failures still fail, and `fail-delta.json` with
`--enforce budget` passes with the delta `IGNORED`. The `pair-*` cases run
`fixtures/gates-pair.toml` (the old `secure_stack / bare ≤ 1.15` pair): `pass.json` exactly at
1.15 passes, `fail-pair.json` (1.156) and `fail-pair-missing.json` fail, and `fail-pair.json`
with `--enforce budget,delta` passes with the pair `IGNORED`. An unknown kind for `--enforce`
exits 2.

## CI job

`.github/workflows/bench.yml` (E2.6, `test.qa#8`) runs on every push to `main` and on pull
requests labelled `perf` (adding the label starts it; later pushes re-run it):

1. Base = the pull request's base commit, or the previous `main` commit on a push (`HEAD^`
   when the push has none).
2. `bench-run.sh --ref <base> --baseline ci-base` (a `git archive` export), then
   `bench-run.sh --baseline ci-head`, one after the other on the **same runner**, criterion
   defaults, into one `target/criterion`.
3. `bench-export.sh` both, then `bench-compare.sh --enforce budget,delta` against
   `gates.toml`. A `budget` or `delta` `FAIL` (or a missing gated id) fails the job; the
   `improve` gates ("must improve on v0.6") are printed as `IGNORED`, since a change compared
   with its parent is near 1.00 by construction ([Gates](#gates)).
4. The two exports and the comparison are uploaded as the artifact `bench-<sha>`.

The runner is the repository variable `BENCH_RUNNER` (a dedicated self-hosted `bench` label)
when set, otherwise `ubuntu-latest`. Shared runners are noisy: a `budget` result within the
0.05 band is `NOISE`; re-run the job before acting on a `FAIL` near a limit. Actions are
pinned by commit SHA and the workflow token is `contents: read`.

## Secure-stack budget (0.11)

The 0.11 security KPI "secure-stack overhead ≤ 5 µs per request over the bare request bench"
(`.maestro/specs/releases/0.11.0.md`, Risk 8) is the `delta` gate
`gate/axum_request_secure_stack − gate/axum_request_bare ≤ 5000 ns`, measured inside one run
(same run, same runner).

The KPI was `secure_stack / bare ≤ 1.15` until the owner replaced it with this absolute budget
(owner decision 2026-10-19). Since `bf7be935` (inline request fibers) a bare in-process
request takes 0.90 µs, so 1.15× would leave about 135 ns for the CSP nonce, CSRF masking,
API-key hashing, log redaction, the guard and rate limiting, which is real per-request work.
The ratio is still printed next to the result, for information; it is not a gate.

| Measured 2026-10-19 | Median |
|---|---|
| `gate/axum_request_bare` | 0.90 µs |
| `gate/axum_request_secure_stack` | 5.55 µs |
| difference (gated, ≤ 5 µs) | +4.65 µs, PASS |
| ratio (informational) | 6.2× |

Both benches (`crates/id_effect_axum/benches/gate_request.rs`) send the same in-process
`GET /pair` (peer address `ConnectInfo`, an `x-api-key` header) to an effect handler answering
`hello`; the secure one goes through, outermost first:

| Layer | Configuration |
|---|---|
| `RateLimitLayer` | per client IP, a bucket that never empties in the bench |
| `LoadShedLayer` | in-flight cap 1024, `503` when full |
| `SecurityLayer::recommended` | CSRF, CSP nonce, security headers, cookie defaults |
| `Authenticate` (id_effect_auth) | API key over `MemoryApiKeyStore` |
| `Guard` (id_effect_authz) | `authenticated()` on the route |
| default log redaction | the handler writes one access line through `EffectLogger`, scrubbed by `RedactionPolicy::recommended` (no policy installed) |

Every layer must let the request through: the bench asserts `200`. The stack grows as the 0.11
security leaves land (the first-party `LoadShedLayer`, CSRF masking, the access layer);
`r11-w3-kpis` re-measures both benches with the final stack for the release figure.

## 0.10 exit (`r10-w3-kpis`)

The release gate is `bench-compare.sh` against 0.9.0 with no `budget` gate failed, with
`id_effect`'s `fiber-registry` feature off and on. No 0.9 export was stored, so `v0.9.json` is
`v0.9.0` (`be54134a`) benched in the same session as 0.10.0 (`c3eb42a1`): the three sides
interleaved per bench (v0.9.0, off, on), criterion defaults, `performance` governor, 1-min load
1.6–4.2. `bench-run.sh` has no feature switch, so the on side ran
`cargo bench --features id_effect/fiber-registry` (`--features fiber-registry` for `id_effect`).

| Comparison | `budget` (7) | `improve` (3) |
|---|---|---|
| `v0.9.json` → `v0.10.json` (off) | all PASS (max `coord/channel_spsc_10k` 1.06) | `spawn_join_10k` 0.84 PASS; `sleep_cpu_1k_x_1ms` 1.02, `axum_request_oneshot` 1.02 FAIL |
| `v0.9.json` → `v0.10-fiber-registry.json` (on) | all PASS (max `coord/queue_bounded_10k` 1.13) | `sleep_cpu_1k_x_1ms` 0.94 PASS; `spawn_join_10k` 1.19, `axum_request_oneshot` 1.01 FAIL |
| `v0.6.json` → off / on (stored baseline, other session) | all PASS (max `bench_schedule_retry_overhead` 1.30 of 2.0) | all PASS |

- **Gate met:** no `budget` FAIL, off or on.
- **`improve` against 0.9:** these gates are "must improve on v0.6" (D9); 0.9 already has the
  improvement (`v0.6` → 0.10: 0.002, 0.002, 0.13), so a 0.9 → 0.10 ratio near 1.00 fails them by
  construction, as a same-build comparison does ([Gates](#gates)).
- **Off → on** (`v0.10.json` → `v0.10-fiber-registry.json` with every gated id a 1.25 budget, the
  `r10-e6-2-fiber-registry` leaf's own check, not `gates.toml`): all within it but
  `gate/spawn_join_10k` 1.42. That bench takes 10 samples and is bimodal on this machine (about
  12 or 17 ms per run; 95 % CIs 9.9–17.4 ms off, 11.8–23.1 ms on). Three more interleaved off/on
  runs of it gave 17.5 / 17.2, 19.2 / 16.7 and 12.1 / 17.7 ms (mean ratio about 1.06), so the
  1.42 is the bimodality, not the registry. `coord/queue_bounded_10k` 1.13 and
  `phase0/bench_env_insert_overhead` 1.09 are the largest on-side costs otherwise.

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

## Feature LOC (K3)

K3 is the number of lines it takes to add one resource to a real service: the "invoice" resource
added to `examples/reference/ledger`, written the way the app writes its accounts (an `Entity`
over a tenant-scoped, soft-deleted, versioned `Repository`, a migration up and down (`0008` at
0.10), typed routes answering Problem+JSON, permissions and a resource policy with a guard on
every route, a background job, an integration test on the app's harness). The job (from 0.10) is
the `InvoiceReminder` on the app's `id_effect_jobs` queue: issuing an invoice enqueues it in the
same transaction (`enqueue_in`), unique per invoice, due 14 days later on the `invoices` queue;
it is registered in the Ledger's job registry and worker and marks a still-unpaid invoice
reminded. It is kept as `k3/invoice.patch`, a unified diff against `HEAD` (repository-relative
paths), not as code in the app.

```bash
bash scripts/dx-kpis.sh --skip-k1 --skip-k2 --skip-k6   # K3 (and K4, K5): the patch applies, its lines counted
ID_EFFECT_REQUIRE_DOCKER=1 bash scripts/dx-kpis.sh --skip-k1 --skip-k2 --skip-k6 --k3-build   # also build it, run its tests
git apply --check docs/benchmarks/k3/invoice.patch   # on a clean checkout of HEAD
```

- **Count:** added (`+`) lines whose content is not blank, comments and doc comments included
  (`#![deny(missing_docs)]` makes them part of the code). A changed line counts as added.
- **Categories** (paths under `examples/reference/ledger`): `migrations/**` migration;
  `tests/**` test (the new test and the existing tests the change has to update, here the
  migration versions in `tests/migrations.rs`); `src/policy.rs` policy; `src/app.rs` routes (the
  router's `merge`); `src/lib.rs` model (`pub mod`); `src/jobs.rs` job (the job type, its
  handler, registration and queue); the resource module `src/<name>.rs` is model up to its first
  private top-level `fn` (the first handler) and routes from there; anything else is `other`.
- **Applies to HEAD:** checked on a scratch index read from `HEAD`
  (`git apply --cached --check`), so local changes do not matter. When the Ledger changes in a way
  that breaks the patch, the script fails: refresh the patch in an export of `HEAD`
  (`git archive HEAD | tar -x -C …`, `git apply`, fix, `git diff`) and re-record K3.
- **`--k3-build`** exports `HEAD` into `DX_KPIS_DIR` (or a temp dir), applies the patch, runs
  `cargo build -p ledger --tests` and `cargo test -p ledger --test invoices` (`SKIP` without
  Docker, a failure with `ID_EFFECT_REQUIRE_DOCKER=1`). `K3_TARGET_DIR` reuses a target dir.
- **The job:** §8 counts model, migration, routes, auth, job and test. 0.9 had no jobs battery, so
  its patch has no job; from 0.10 it has. The totals are therefore not comparable line for line;
  compare per category (the job adds its own `job` category plus a few lines in the others: the
  enqueue in the `PATCH` handler, the `reminded_at` column, its test).
- **Target:** none at 0.9 or 0.10 (measured); §8 sets −40 % against the 0.9 figure.

0.9 (`dx-kpis-0.9.json`): **508** lines over 8 files: model 180, migration 17, routes 153,
policy 30, test 128. The resource module is 330 of them (179 model, 151 handlers and routes).

0.10 (`dx-kpis-0.10.json`): **632** lines over 9 files: model 183, migration 19, routes 172,
policy 30, job 43, test 185. Without the job's own lines (job 43, and in the other categories
the enqueue, the column and the reminder test) the resource is the 0.9 one, re-based on the 0.10
Ledger (migration `0008`, eight migrations in `tests/migrations.rs`).

## Rebuild and release build (K2, K6)

K2 is the edit-build loop of a new service, K6 its cold release build (PLAN-OF-PLANS §8). Both
run on one project generated by `id-effect new svc --template service --local-workspace <repo>`
in `DX_KPIS_DIR` (with the root `Cargo.lock`), with the method of `scripts/compile-time.sh`.

```bash
DX_KPIS_RELEASE=0.10 bash scripts/dx-kpis.sh               # K1, K2, K6 and the static KPIs
K2_RUNS=5 bash scripts/dx-kpis.sh --skip-k1 --skip-k6 --out FILE   # K2 only (5 runs), not the committed file
```

- **Toolchain as shipped:** `RUSTC_WRAPPER`/`RUSTC_WORKSPACE_WRAPPER` (and the `CARGO_BUILD_`
  forms) and `CARGO_INCREMENTAL` are unset, so sccache does not make a build warm and the dev
  profile's own incremental default applies.
- **K2:** the project is built once, untimed. Each run then restores the generated `src/lib.rs`,
  edits the body of the `hello` handler (the greeting `"hello, {}"` becomes `"hello <run>, {}"`,
  a different edit every run) and times `cargo build`. The figure is the median of `K2_RUNS`
  runs (default 3). The script fails when the template no longer has that handler line.
- **K6:** the generated `src/lib.rs`, an empty target dir, one timed `cargo build --release`.
- **Budgets:** K2 ≤ 8 s (`K2_BUDGET_S`, the 0.10 exit KPI; §8 sets ≤ 5 s for 1.0); the script
  fails above it. K6 ≤ 120 s, set at 0.10 (§8): below the K1 budget of 180 s, which a cold dev
  build has to meet with the boot included. K6 is recorded, not checked by the script; compare
  the `k6.seconds` of `dx-kpis-<release>.json` with this budget.
- Run on an idle machine: the load average at the start and the end goes into `machine`.

0.10 (`dx-kpis-0.10.json`, no `CARGO_BUILD_JOBS` cap, 1-min load 2.10 at the start): K1
**19.9 s** (build 19.73 s; budget 180 s, re-measured idle after the loaded 67.6 s of 0.9), K2
**0.97 s** (runs 0.96, 0.97, 1.05; budget 8 s), K6 **22.52 s** (budget 120 s).

## Spike gate (historical)

`SPIKE-REPORT.md` and `spike-gate/` were measured on the two kernel spikes under `spikes/`,
which `k-ui-suite` deleted once design A landed in `id_effect::next`. To reproduce them,
export the pinned spike tree (`git archive 9cf380e | tar -x -C .tmp/spike-9cf380e`) and run
the spike's own bench instructions from `spikes/README.md` in that export. The kernel gates
are now `gates-k.toml`, measured on `id_effect::next` (`k-runloop-opt.md`).
