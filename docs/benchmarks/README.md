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
| `v0.11.json`, `v0.12.json` | The 0.12 bench gates: the 0.11.0 tree (`release/0.11.0`, a `git archive` export) and 0.12, one session; `v0.12.json` also carries the load, CPU and toolchain of every run, the HTTP/3 trials and the soak ([RPC gates](#rpc-gates-012)) |
| `v0.13.json` | The 0.13 breadth benches (`r13-e-benches`): every gate bench on the 0.13 tree, with the measurements the D15 budgets of the i18n and UI benches are written from ([Breadth gates (0.13)](#breadth-gates-013)) |
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
| `delta` | `rpc/unary_json` over `rpc/axum_json_handler` | new − new ≤ 3000 ns (no noise band; [Unary budget (D5)](#unary-budget-d5)) |
| `pair` | `rpc/batch_50` over `rpc/sequential_50` | new / new ≤ 0.20 (no noise band; [RPC gates](#rpc-gates-012)) |
| `pair` | `h3_loss/h3_p99` over `h3_loss/h2_p99` | new / new ≤ 1.00 (noise 0.05; netem in CI, [HTTP/3 under loss](#http3-under-loss-d9)) |
| `pair` | `orm/eager_100x10` over `orm/n_plus_one_100x10` | new / new ≤ 0.25 (no noise band; [Breadth gates (0.13)](#breadth-gates-013)) |
| `delta` | `i18n/format_plural` over `i18n/empty_iter` | new − new ≤ 496 ns, the measured median × 1.5 (D15; no noise band; [Breadth gates (0.13)](#breadth-gates-013)) |
| `delta` | `ui/render_list_50` over `ui/empty_iter` | new − new ≤ 14,710 ns, the measured median × 1.5 (D15; no noise band) |
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
  is gated in that change already. From 0.12 the batch gate and the HTTP/3 gate are pairs
  (two measurements of the same stack); the self-test covers the kind with
  `fixtures/gates-pair.toml`, and every fixture carries the four ids of the two pairs, and
  the two of the unary delta, at passing values. From 0.13 the eager-loading gate is a third
  pair, and every fixture also carries `orm/eager_100x10` and `orm/n_plus_one_100x10` (0.10).
- **Delta gates** (`kind = "delta"`, `over = "<id>"`) are an absolute difference inside one
  run: `new[id] - new[over]` in the metric's unit (ns), against `limit` in ns. The `RATIO`
  column then shows the difference (`+4650`), and the result the ratio for information
  (`PASS (ratio 6.17)`). The global 0.05 band does not apply; only a `noise` set on the gate
  (in ns) does. From 0.13 the D15 budgets are delta gates over an empty iteration
  (`i18n/empty_iter`, `ui/empty_iter`, about 0.2 ns), which makes them absolute budgets on the
  bench's own median that need no base; every fixture carries their four ids at passing values.
- **`--enforce KINDS`** (comma list of `budget`, `improve`, `pair`, `delta`): only those
  kinds' missed limits `FAIL`; the others print `IGNORED`. A missing id still `FAIL`s. The CI
  job uses `--enforce budget,delta,pair`.
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
| `id_effect_rpc` (feature `postcard`) | `gate_rpc` | `rpc/unary_json`, `rpc/axum_json_handler`, `rpc/unary_postcard`, `rpc/sequential_50`, `rpc/batch_50` |
| `id_effect_rpc` (feature `http3`) | `h3_loss` (no criterion harness; writes criterion estimates) | `h3_loss/h2_p99`, `h3_loss/h3_p99`, `h3_loss/h2_p50`, `h3_loss/h3_p50` |
| `id_effect_sql` (features `repository,sqlite`) | `repository_eager` | `orm/eager_100x10`, `orm/n_plus_one_100x10`, `orm/flat_100x10` |
| `id_effect_i18n` | `format` | `i18n/format_plural`, `i18n/empty_iter` |
| `id_effect_axum` (feature `ui`) | `ui_render` | `ui/render_list_50`, `ui/empty_iter` |

`scripts/bench-run.sh` lists each bench as `package:bench[:features]` and passes the features to
`cargo bench`. The 10,000-stream soak is an example, not a bench:
`cargo run --release -p id_effect_rpc --features ws --example streams_soak -- --streams 10000 --minutes 10`
([Stream soak](#stream-soak-t12)).

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
   Both runs leave out `h3_loss` (`--skip id_effect_rpc:h3_loss`).
3. HTTP/3 under loss, head only: the step puts `tc netem loss 1% delay 5ms limit 100000` on
   `lo` (with `sudo`; the loopback MTU set to 1500 and TSO/GSO/GRO off first, so netem sees
   MSS-sized TCP packets), runs `H3_LOSS_NETEM=1 bench-run.sh --baseline ci-head --only
   id_effect_rpc:h3_loss`, and removes the qdisc on exit. Nothing else runs under netem.
4. `bench-export.sh` both, then `bench-compare.sh --enforce budget,delta,pair` against
   `gates.toml`. A `budget`, `delta` or `pair` `FAIL` (or a missing gated id) fails the job;
   the `improve` gates ("must improve on v0.6") are printed as `IGNORED`, since a change
   compared with its parent is near 1.00 by construction ([Gates](#gates)). The pair gates read
   the head export only, so a base without the RPC benches is fine.
5. The two exports, the comparison and the HTTP/3 trials are uploaded as the artifact
   `bench-<sha>`.

Two more jobs run the [stream soak](#stream-soak-t12): `soak` (10 minutes, beside the bench job:
pushes to `main` and `perf` pull requests; the result JSON is the artifact `soak-<sha>`) and
`soak-60s` (60 s on every pull request).

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

| Measured 2026-10-19 (criterion defaults) | Before the trim | After |
|---|---|---|
| `gate/axum_request_bare` | 0.91 µs | 0.91 µs |
| `gate/axum_request_secure_stack` | 5.42 µs | 3.54 µs |
| difference (gated, ≤ 5 µs) | +4.51 µs, PASS | +2.63 µs, PASS |
| ratio (informational) | 6.0× | 3.9× |

The trim took the difference from +4.5 µs to +2.6 µs without changing what any layer checks:
`SecurityLayer` renders the CSP header once per policy and fills in only the nonce, signs the
CSRF cookie with an HMAC keyed once per layer and masks the token only when it is read; the
API-key check and a pure guard policy (`authenticated()`, roles, predicates) decide inline
instead of on a fiber (`ApiKeyStore::find_now`, `Policy::decide_now`; a store or policy that
needs I/O still runs as an effect, and a guard with audit events always does); the logger reads
no fiber ref that is not installed and formats no line `tracing` would drop.

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

## RPC gates (0.12)

The T12 gates of `.maestro/specs/releases/0.12.0.md` (`r12-e12-5-benches`). Ratios are kept only
between two measurements of the same stack (batch against sequential, HTTP/3 against HTTP/2);
where a ratio is ill-posed the budget is absolute (the unary RPC delta, D5; the secure-stack
delta, 0.11).

Measured 2026-10-19 on the reference machine (AMD Ryzen 9 7950X, 32 threads, `performance`
governor, `rustc 1.99.0-nightly (af3d95584 2026-07-09)`, Linux 6.18.55), shared with two to
three other agents. Every final number comes from a bench file started at a 1-minute load
average below 3.5 and finished below 4 (polled; each run's window and load are in
`v0.12.json` `.session.runs`); runs that crossed 4 were thrown away and repeated. The benches ran
on `5d9119d5` plus this leaf's files, the gated soak on `82beb0f2` (two-way resume) plus them; the base is `release/0.11.0` (`037dffc4`), in the same
session.

| Gate | Kind | Measured (median) | Limit | Headroom | Spread seen (5 runs, load 2–9) |
|---|---|---|---|---|---|
| `rpc/unary_json − rpc/axum_json_handler` (D5) | `delta`, noise 0 | 2,725 − 949 = **+1,776 ns** (ratio 2.87; +3,399 ns before `r12-f-unary-perf`) | 3,000 ns | 1,224 ns | +1.75 … +1.80 µs |
| `rpc/batch_50 / rpc/sequential_50` | `pair`, noise 0 | 180.5 µs / 1,626.9 µs = **0.111** (9.0× faster) | 0.20 | 0.089 (45 % of the limit) | 0.102 … 0.120 |
| `gate/axum_request_secure_stack − bare` (0.11, unchanged) | `delta`, noise 0 | 3,576 − 929 = **+2,648 ns** | 5,000 ns | 2,352 ns | +2.58 … +2.65 µs (+2.88 µs at load 16, discarded) |
| `h3_loss/h3_p99 / h3_loss/h2_p99` (D9) | `pair`, noise 0.05 | 38.5 ms / 589.0 ms = **0.065** (userspace proxies) | 1.00 | 0.935 | 0.05 (F22 spike) … 0.065 |

- **Noise headroom.** The batch pair has no noise band and sits at about half its limit; its
  worst run (0.120, under load 9) is still 40 % below. The secure-stack delta keeps 2.35 µs of
  its 5 µs, about ten times the run-to-run spread. The HTTP/3 pair carries the global 0.05 band
  because each trial's p99 is one quantile of 12,800 samples; locally its trials spread
  28.5–38.9 ms for HTTP/3 and 587–600 ms for HTTP/2, far inside the limit. On the shared CI
  runner re-run before acting on a result near a limit ([CI job](#ci-job)).
- **`rpc/unary_postcard`** (report only): 4,465 ns, 59 ns more than JSON for this three-field
  struct; 3,346 ns after `r12-f-unary-perf` (621 ns more than JSON: the typed bridge decodes the
  postcard input to `T` and re-encodes it as the JSON value the Schema then decodes, and the
  reverse for the output).

### Unary budget (D5)

`rpc/unary_json` is a `#[rpc::service]` query (`Bench.echo`, a struct of an `i64`, a `String`
and a `bool`) called as `POST /rpc/Bench.echo` through `oneshot` on the mounted router, as the
`gate_request` benches; `rpc/axum_json_handler` is a plain axum handler doing the same serde
decode and encode (`Json<Item>` → `Json<Item>`). The `delta` gate (`gates.toml`, limit 3,000 ns,
noise 0) holds since `r12-f-unary-perf`.

`r12-e12-5-benches` measured **+3.40 µs** (+3.38 to +3.59 µs over the day), 0.4 µs over the
budget, and left the gate out. `r12-f-unary-perf` profiled the call (`perf record` on the bench,
then a counting global allocator timing each stage in isolation; `v0.12.json`
`.session.unary_perf.profile`) and removed per-call copies that check nothing:

| Removed per call | Cost |
|---|---|
| Two `WireSchema` clones (input and output), each a deep copy of its JSON Schema `Value` (now an `Arc`) | ~690 ns, 48 allocations |
| A copy of the decoded input `Value` before the Schema decode (`WireSchema::decode_value` takes it by value) | ~64 ns, 5 allocations |
| String and key copies turning the encoded `Unknown` back into a `Value`; an `RpcServer` clone per request (now an `Arc`) | 4 allocations |
| Two key `String`s per procedure lookup (`RpcGroup` is keyed by tag, then version); the JSON encode buffer growing | 2 allocations |

150 allocations (21.7 KB) per call became 91 (13.7 KB); the raw handler makes 32. Every check
stays: `x-rpc`, the codec `Content-Type`, the body cap, the procedure's guards, the deadline
clamp and race, the Schema decode collecting every issue, `InternalError` redaction and the
idempotency store.

Five runs at load 2.4–3.2 (23:17–23:20 and 22:57–22:59, tree `a0ef408a` plus the leaf; four
runs that ended above load 4 were discarded): +1,746, +1,773, +1,776, +1,781 and +1,805 ns,
median **+1,776 ns** (2,725 − 949; `rpc/unary_postcard` 3,346 ns). Where the call's time goes
now, by stage (each timed alone, ns): the router with the deny-by-default access layer
947 (axum alone 831), the Schema decode of the input (`Value` → `Unknown` → `T`) 380, the effect
fiber (`run_with_env`) 328, the Schema encode of the output 240, the body to a `Value` 190, the
`WireResult` envelope 90, the correlation id 45, the deadline race 30.

### HTTP/3 under loss (D9)

`benches/h3_loss.rs`: 64 multiplexed `GET`s of 16 KiB on one connection per round, 200 rounds
after 10 warm-up rounds, 3 trials alternating HTTP/2 and HTTP/3 on fresh connections; a sample
is one request's latency, the gated figure the median over trials of each trial's p99. The
server is the shipped host (`ServerHandle` with TLS: HTTP/2 on TCP, HTTP/3 on the same UDP port
through the platform `h3` route, BBR by default); the clients are hyper's HTTP/2 client and `h3`
on the platform HTTP/3 client config (BBR, 4 MiB socket buffers).

- **CI (the gate):** `bench.yml` runs it under real `tc netem loss 1% delay 5ms` on `lo`
  (`H3_LOSS_NETEM=1`, [CI job](#ci-job)). Locally `sudo` is blocked, so this design has **not
  been run here**; the first CI run on the bench runner (owner step D16) is the first netem
  figure. Two caveats of netem on loopback, both documented in the job: the MTU (65,536 on `lo`)
  and TSO/GSO would let one "packet" carry 64 KiB of TCP, so the job sets MTU 1500 and turns
  them off; quinn's UDP GSO still hands netem batches of datagrams as one packet, so HTTP/3's
  losses come in bursts (harsher for HTTP/3, not kinder).
- **Locally:** userspace proxies (the D9 spike's, F22): a UDP proxy drops 1 % of datagrams per
  direction and delays the rest 5 ms; a TCP proxy delays MSS-sized segments 5 ms and models a
  lost one as a fast retransmit 12.5 ms later with head-of-line blocking and a 0.7 window cut
  (generous to TCP: no RTO, ACK loss free).

| 2026-10-19, userspace proxies, load 2.4–3.3 | p50 | p99 (median of 3) | trials p99 |
|---|---|---|---|
| HTTP/2 over TLS | 245 ms | 589.0 ms | 599.7, 589.0, 587.1 ms |
| HTTP/3, BBR (shipped) | 15.7 ms | 38.5 ms | 38.5, 28.5, 38.9 ms |
| h3 / h2 | | **0.065** | |

No UDP receive-buffer drops in any trial. BBR wins clearly in the model, and BBR does not react
to random loss by design, so it is expected to hold under netem; the gate therefore stays an
enforced `pair`. Should the netem job show HTTP/3 losing, D9 makes the gate report-only
(`kind = "report"`, both ids listed) and F36 applies: a congestion-control switch in
`Http3Options` so operators can fall back to Cubic without code (the axum host registers
`Route::h3()`, BBR, unconditionally today).

### Stream soak (T12)

`examples/streams_soak.rs` (feature `ws`): a child process serves one node (the host's
`serve_listener` with default `ConnectionLimits`, an `RpcServer` with the default
`RpcWsConfig`); the parent opens 10,000 `Soak.beats` server streams, 50 per socket on 200
sockets spread over source addresses `127.0.0.2`… so no address passes the 16-sockets-per-IP
default, and each stream gets `{seq, at_ms}` every second. The node's `VmRSS` is sampled every
30 s. Pass: every stream received every heartbeat due (no gap, duplicate or early end) and
RSS at the end ≤ 1.05 × RSS at minute 5 (half a shorter run: 30 s of the 60 s variant). Since
0.12.0 (F60) the mark is minute 5, after the replay buffers' warm-up below; the RSS at minute 1
and the growth since then are still reported (`rss_kib.at_minute_1`,
`rss_kib.growth_since_minute_1`; `scripts/security-kpis.sh` reports `rss_growth_minute_1` and
gates the minute-5 figure, read from the samples for runs recorded with the old minute-1 mark).

The gated run is on `82beb0f2` (two-way resume landed: every resumable stream's items stay in
the socket's replay buffer until the client's next credit grant):

| 2026-10-19, 10,000 streams, 10 min (21:38:32–21:48:32), tree `82beb0f2` | Value |
|---|---|
| heartbeats received / missing / out of order / streams ended early | 6,009,790 / 0 / 0 / 0 |
| node RSS at minute 1 → minute 10 | 341,648 → 355,280 KiB (**1.040×**, limit 1.05; peak 357,176 at 8.5 min) |
| node RSS once all streams were open (0.1 s) / at 30 s | 202,628 / 330,496 KiB |
| heartbeat lag p50 / p99 | 0 ms / 41 ms |
| 1-minute load | 3.93 at the start, 1.4–6.5 during |

| Same soak before two-way resume (tree `5d9119d5`, 20:53:55–21:03:55, load 1.8–4.5 for 7 min, then 13–17) | Value |
|---|---|
| heartbeats received / missing | 6,009,795 / 0 |
| node RSS at minute 1 → minute 10 | 181,996 → 180,600 KiB (0.992×) |

- **Thin headroom (1.040 of 1.05).** With the resume replay buffers the node holds about 35 KiB
  per stream instead of 16 KiB, and RSS keeps climbing slowly until about minute 5 (345 MB at
  minute 1.5, 356 MB at minute 5, flat after). The buffers are bounded (per session 1,024
  frames, 1 MiB, 30 s; `resume::ResumeConfig`), so this is warm-up, not a leak, but a minute-1
  mark sits inside it. The gate passes; a 10-minute CI run on a noisier runner could cross 1.05.
  Decided at r12-w8-kpis (F50/F60): the mark moved to minute 5 (this run: 356,240 → 355,280 KiB,
  **0.997×**) rather than shrinking the replay window, which would weaken resume; the minute-1
  figure (1.040×) stays in the report.
- The 42 ms p99 lag is Nagle's algorithm meeting delayed ACKs: neither the host listener nor
  `axum::serve` sets `TCP_NODELAY`, so a small WebSocket frame written while the previous one is
  unacknowledged waits for the peer's delayed ACK (about 40 ms). A 300-stream, 20 s run with
  `TCP_NODELAY` on the node's sockets shows p99 1 ms (42 ms without). Recorded as a follow-up for the host, not a soak
  failure. Fixed by `r12-f-unary-perf` (F48): the host's accept loop (`serve_listener`,
  `serve_router*`, `ServerHandle`) sets `TCP_NODELAY` on every accepted socket; a 60 s soak of
  10,000 streams then shows lag p50 / p99 / p999 0 / 1 / 1 ms (23:23, load 3.3 → 2.1, 609,831
  heartbeats, none missing, RSS 1.020×; `v0.12.json` `.session.unary_perf.soak_60s_after_f48`).
- 100 streams per socket also pass (1,000 streams, 20 s): opening 100 calls uses exactly the
  100-message burst of the socket's inbound rate, and the credit frames of 1 Hz streams stay
  near 6 per second, so F39 (credit frames counted against the message rate) does not bite at
  this rate.
- The 60 s variant (`--seconds 60`, RSS mark at 30 s, after the first credit grants) runs on
  every pull request; on `82beb0f2` 30 s → 60 s is 1.034× (330,496 → 341,648 KiB in the 10-minute
  run; 332,804 → 344,000 KiB in a 60 s run at load 20–30). With the old 10 s mark it failed
  (1.167×) by construction, so the mark moved. The 10-minute run is the `soak` job in
  `bench.yml`.

### 0.12 against 0.11.0

`bash scripts/bench-compare.sh docs/benchmarks/v0.11.json docs/benchmarks/v0.12.json`: no
`budget`, `delta` or `pair` gate fails. The three `improve` gates fail against 0.11 by
construction (they are "must improve on v0.6"; 0.11 already has the improvement), as at the
0.10 exit; against `v0.6.json` all three pass (0.002, 0.002, 0.014).

| Comparison | `budget` (7) | `delta` / `pair` | `improve` (3) |
|---|---|---|---|
| `v0.11.json` → `v0.12.json` | all PASS (max `bench_schedule_retry_overhead` 1.004; `channel_spsc_10k` 0.86) | secure stack +2,648 ns PASS, batch 0.111 PASS, h3 0.065 PASS | 0.96, 1.02, 1.02 FAIL (by construction) |
| `v0.6.json` → `v0.12.json` | all PASS (max `bench_schedule_retry_overhead` 1.31 of 2.0) | as above | all PASS |

## Breadth gates (0.13)

The bench gates of `.maestro/specs/releases/0.13.0.md` (`r13-e-benches`, decision D15). First
measured 2026-10-22 00:13–00:45 UTC on the reference machine (AMD Ryzen 9 7950X, 32 threads,
`performance` governor, `rustc 1.99.0-nightly (af3d95584 2026-07-09)`, Linux 6.18.55), tree
`1a7dcb24` plus this leaf's benches, criterion defaults, with `bash scripts/bench-run.sh
--baseline v0.13`; the D15 budgets are written from that run. **Not an idle machine:** other
agents were compiling the workspace, the 1-minute load ranged 15–170 during the run (36 at the
start, 20 at the end). The ratios below are within one run and held across three runs; the
absolute medians are an upper bound.

`v0.13.json` is the export of the leaf's Gate command run end to end on 2026-10-24
08:26–08:43 UTC, tree `70f407ad` (`release/0.13.0`), same machine and toolchain:
`bash scripts/bench-run.sh --baseline v0_13_r4`, `bench-export.sh`, then
`bash scripts/bench-compare.sh docs/benchmarks/v0.12.json` on the export ([0.13 against
0.12](#013-against-012)). It is the quietest of the three gate runs: 1-minute load 9–20 (sampled
every 30 s), apart from 08:36–08:39 UTC when the run's own `h3_loss` build and proxies took it to
47–103, and 11–20 while `orm/*`, `i18n/*` and `ui/*` ran. Its `.session` keeps that run's load and
figures, and the two earlier gate runs' (2026-10-24 07:48–08:11 UTC, tree `f57f10ba`, load
21–180; 2026-10-22, tree `1a7dcb24`, load 15–170) under `.session.earlier`. It is still not the
idle machine the leaf asks for: two unrelated single-thread processes held a core each and the
other leaves' agents share the machine during the 0.13.0 build-out, so an idle-machine (or
`BENCH_RUNNER`) run is still owed, by the owner or the exit leaf.

| Bench (`package`, `benches/…`) | What one iteration does |
|---|---|
| `orm/eager_100x10` (`id_effect_sql`, `repository_eager.rs`, features `repository,sqlite`) | `Repository::with(Order::lines()).find_all()`: 100 orders, then their 1,000 lines in one `IN (…)` query (2 queries), tenant and soft-delete scoping on, rows decoded into `Vec<Loaded<Order>>` |
| `orm/n_plus_one_100x10` (same) | the same 100 orders, then one `find_by(order_id = ?)` per order in sequence (101 queries, `order_id` indexed) |
| `orm/flat_100x10` (same, report only) | the same rows as two plain `find_all()` reads, with no relation work: the floor eager loading cannot go below on this driver |
| `i18n/format_plural` (`id_effect_i18n`, `format.rs`) | `Catalog::format` of a plural message with two arguments in `de-AT` (falls back to `de`), counts cycling 1, 2, 5, 21, bidi isolation on |
| `ui/render_list_50` (`id_effect_axum`, `ui_render.rs`, feature `ui`) | a full page to a `String`: a list component with 50 row components (as slots), each with a checkbox and an htmx delete button and two values maud must escape, inside `BasicLayout` |
| `i18n/empty_iter`, `ui/empty_iter` (same binaries) | `black_box(())`: the harness floor the two D15 budgets are measured over |

All of them run on SQLite in memory or in process; every gate id is in `gates.toml`, the benches
in `scripts/bench-run.sh`.

| Gate | Kind | Measured (median) | Limit | Result |
|---|---|---|---|---|
| `orm/eager_100x10 / orm/n_plus_one_100x10` | `pair`, noise 0 | 1.192 ms / 3.186 ms = **0.374** (re-runs: 0.366, 0.349, 0.334, 0.313; end-to-end gate runs 1.330 / 3.195 ms = 0.416 and, in `v0.13.json`, 1.160 / 3.333 ms = 0.348) | 0.25 | **FAIL** (see below) |
| `i18n/format_plural − i18n/empty_iter` | `delta`, noise 0 | **330.9 ns** (re-run 321.8 ns, end-to-end 329.3 and 326.6 ns; empty iteration 0.19–0.26 ns) | 496 ns (330.9 × 1.5, D15) | PASS; owner confirms at exit |
| `ui/render_list_50 − ui/empty_iter` | `delta`, noise 0 | **9,807 ns** (re-run 10,136 ns, end-to-end 10,091 and 10,118 ns; empty iteration 0.19 ns) | 14,710 ns (9,807 × 1.5, D15) | PASS; owner confirms at exit |

**The eager-loading pair misses 0.25, and the driver sets the floor.** Eager loading does what it
should (2 queries instead of 101, 2.7× faster than the N+1 loop), but on `SqliteSqlClient` the
rows, not the round trips, dominate: sqlx-sqlite returns 1,000 rows in about 670 µs (0.67 µs a
row, measured with `sqlx::query(…).fetch_all` alone) and a 10-row query about 30 µs. With
10 children per parent the N+1 loop spends about 2.3 ms on its 99 extra queries and 0.9 ms on the
same 1,100 rows eager loading reads, so even `orm/flat_100x10` (no relation work at all) is
0.290 of `n_plus_one`. Eager loading adds about 270 µs over `flat` (1.192 against 0.925 ms): the
owner key column decoded and rendered to a `String` per child row, the `HashMap` grouping and
the 100-value `IN` list. Reaching 0.25 needs both a cheaper relation pass (`repository/relations.rs`)
and fewer microseconds per row in the SQLite driver (`sqlite/`), neither in this leaf's files; or
an owner decision on the limit (D15 wrote it before measuring). The gate stays at the decided
0.25 and fails until one of those lands. It is a `pair`, so it reads the head export only, and
`bench.yml` (`--enforce budget,delta,pair`) fails on it as well. A re-run on 2026-10-22 (01:00 UTC,
1-minute load 16–30, 1 s warm-up, 4 s measurement) gave the same picture: eager 1.194 ms, N+1
3.259 ms, flat 0.847 ms, so eager / N+1 = 0.366 and flat / N+1 = 0.260. Even the floor misses 0.25.
A third run (01:12 UTC, criterion defaults, 1-minute load 29–145, tree `ed8c17ef` plus the
empty-iteration benches) gave eager 1.128 ms, N+1 3.234 ms, flat 0.937 ms: 0.349, floor 0.290.
With raw sqlx at about 0.67 µs a row, two statements over 1,100 rows cost about 0.8 ms, already a
quarter of the N+1 loop, so no change in `relations.rs` alone can reach the limit, and no change
in this leaf's files (the bench shape is fixed by the leaf: 100 × 10, SQLite in memory) can
either.

A fourth run on 2026-10-24 (current `release/0.13.0` tree, 1-minute load about 108, 1 s warm-up,
3–4 s measurement) gave eager 1.244 ms, N+1 3.726 ms, flat 0.938 ms: **0.334**, floor 0.252. Two
bench-side knobs were tried and rejected, since neither changes what the gate compares and
neither helped: a sqlx row channel of 2,048 rows instead of 50 (`row_buffer_size`, so the
1,000-row statement never waits on the consumer) made eager slower (1.370 ms against 3.405 ms,
0.402), and a current-thread tokio runtime instead of the multi-thread one gave 1.191 ms against
3.459 ms (0.344). The bench keeps the default pool and the multi-thread runtime a server uses.

The first end-to-end gate run of 2026-10-24 (07:48 UTC, now `.session.earlier` in `v0.13.json`,
1-minute load 21–180) gave eager 1.330 ms,
N+1 3.195 ms, flat 0.936 ms: **0.416**, floor 0.293. Eager's confidence interval was wide
(1.57–2.00 ms on the mean against N+1's 3.22–3.31 ms): its 1,000-row statement is the one most
exposed to a busy machine, so the ratio rises with load rather than falling. Nothing in the client
between the repository and sqlx can be shaved from the bench side either: `SqliteSqlClient::query`
(`crates/id_effect_sql/src/sqlite/client.rs`) is one sqlx `fetch_all` followed by `decode_rows`,
so the per-row cost the floor measures is the sqlx-sqlite driver's (its worker thread and row
channel), and reaching 0.25 on this shape means a different SQLite driver path, not a tuning knob.

The quietest runs so far, both on 2026-10-24 with tree `70f407ad` and no change to the bench:
a single-bench run at 08:25 UTC (1-minute load 10–14) gave eager 1.196 ms, N+1 3.823 ms, flat
1.013 ms: **0.313**, floor 0.265; the end-to-end gate run stored as `v0.13.json` (08:26–08:43
UTC, load 11–20 while `orm/*` ran) gave eager 1.160 ms, N+1 3.333 ms, flat 0.838 ms: **0.348**,
floor 0.251. Lower load lowers the ratio, as the 0.416 run suggested, but not to the limit.
Eager loading reads the same rows as `flat` and does strictly more with them, and in the seven runs
it cost 0.05–0.12 of the N+1 loop more than `flat`, while `flat / N+1` itself read 0.251–0.293
and never below the limit. An idle machine is unlikely to change the verdict; it is still owed
for the absolute numbers (decision 3).

**Open owner decisions (0.13.0 exit, with D15).** None of them is this leaf's to take; each is
listed in the leaf's report so the exit leaf can put it to the owner.

1. *Eager pair limit* (release spec `bench_gates.eager`, exit table "eager ≤ 0.25", leaf criterion
   1). Keep 0.25 and add a leaf for the SQLite driver's per-row cost
   (`crates/id_effect_sql/src/sqlite/`, which no 0.13 leaf owns; `decode` in `value.rs` copies the
   storage-class name into a `String` for every value read, a first candidate). Or set the limit
   from the measurement: the seven runs so far read 0.313–0.416, all on a shared machine, so a
   limit from them (0.45 would hold every one and still require eager loading to beat the N+1 loop
   by 2.2×) should be confirmed by an idle-machine or `BENCH_RUNNER` run first. Or change the shape
   so round trips dominate as they do on a networked database: more parents with fewer children,
   or PostgreSQL. The gate is not changed without that decision, so until then the leaf's Gate
   command exits 1, and this pair is its only `budget`, `delta` or `pair` failure (the three
   `improve` gates fail by construction, below).
2. *D15 budget numbers and kind* (release spec owner item 8). 496 ns and 14,710 ns are the first
   measurement × 1.5 on the reference machine under load (above); every later run passes them
   (i18n 322–329 ns, UI 10,091–10,136 ns). They are absolute, like the secure-stack and unary RPC
   budgets, so a slower `BENCH_RUNNER` may need them re-measured there; the owner confirms them or
   replaces them with a runner measurement at the 0.13.0 exit, together with the `delta` kind that
   carries them (below) in place of the `budget` kind the leaf row names.
3. *Idle-machine measurement.* The leaf asks for an idle machine; the 0.13.0 build-out shares the
   reference machine with every other leaf's agent, so the quietest run here (load 9–20) is the best
   this leaf could get. The owner or the exit leaf re-runs the Gate command on an idle machine or
   on `BENCH_RUNNER` and replaces `v0.13.json` with that export.

The eight `fixtures/*.json` edits (each fixture carries the eager pair's two ids and the D15
gates' four at passing values so `--self-test` stays green) fall under `docs/benchmarks/**`, which
the release spec's "File ownership" table gives to `r13-e-benches` (W4). The leaf row's own
Owns list names only `docs/benchmarks/{gates.toml,README.md,v0.13.json}`; without the fixture
edits, every new gate id would be missing from the synthetic exports and `--self-test` would
fail, so they are part of adding the gates. Listed here for the owner's review at exit.

**Why the D15 budgets are `delta` gates over an empty iteration.** `gates.toml` is compared
against `v0.12.json` (the release gate) and, in CI, against the parent commit; a `budget` or
`report` id must exist in both files, and neither has these benches, so such a row would fail on
the change that adds them, and a ratio against `v0.13.json` itself proves nothing. A `delta` reads
the new export only: `new[id] − new[empty_iter] ≤ limit` in ns is "median ≤ measured median ×
1.5" up to the 0.2 ns floor, with no base. So both budgets are enforced wherever `gates.toml` is
read: the release gate (`bench-compare.sh docs/benchmarks/v0.12.json NEW`) and `bench.yml`
(`--enforce budget,delta,pair`), with no change to either script. Each bench binary has its own
empty iteration, so `--only` one of them still finds both ids. This departs from the row's wording
(`a budget gate … at measured p50 × 1.5`): the number is that budget, the kind is `delta`
because `bench-compare.sh`'s `budget` kind is a ratio against a base that has the bench. The
owner confirms the kind with the numbers at the 0.13.0 exit (decision 2 above).

### 0.13 against 0.12

`bash scripts/bench-compare.sh docs/benchmarks/v0.12.json docs/benchmarks/v0.13.json` (base from
2026-10-19, other session): every 0.12 gate unchanged and green apart from the `improve` gates,
which fail against a release that already has the improvement, as at the 0.10 and 0.12 exits.

| Comparison | `budget` (7) | `delta` / `pair` (0.12) | `improve` (3) | new `pair` | new `delta` (D15) |
|---|---|---|---|---|---|
| `v0.12.json` → first run (2026-10-22, `1a7dcb24`) | all PASS (max `coord/queue_bounded_10k` 1.225) | secure stack +2,759 ns PASS, unary RPC +1,782 ns PASS, batch 0.092 PASS, h3 0.063 PASS (userspace proxies) | 0.856 PASS, 0.978 and 1.031 FAIL (by construction) | eager 0.374 FAIL | i18n +331 ns, UI +9,807 ns PASS |
| `v0.12.json` → first end-to-end gate run (2026-10-24 07:48 UTC, `f57f10ba`, load 21–180; exit 1) | all PASS (max `coord/queue_bounded_10k` 1.082) | secure stack +2,659 ns PASS, unary RPC +1,812 ns PASS, batch 0.082 PASS, h3 0.047 PASS (userspace proxies) | 0.956, 1.014 and 1.040 FAIL (by construction) | eager 0.416 FAIL | i18n +329 ns, UI +10,091 ns PASS |
| `v0.12.json` → `v0.13.json` (end-to-end gate run, 2026-10-24 08:26 UTC, `70f407ad`, load 9–20; exit 1) | all PASS (max `coord/channel_spsc_10k` 1.116) | secure stack +2,698 ns PASS, unary RPC +1,768 ns PASS, batch 0.086 PASS, h3 0.064 PASS (userspace proxies) | 1.103, 0.984 and 0.990 FAIL (by construction) | eager 0.348 FAIL | i18n +327 ns, UI +10,118 ns PASS |

The streaming group against 0.12, for `r13-f-stream-chunking` (pull path only, "batch throughput
unchanged within noise"): `stream/pull_map_10k` 0.998, `stream/map_effect_1k` 0.997,
`phase0/bench_stream_throughput_collect` 1.017 in the first run; 1.005, 1.012 and 1.050 in the
first end-to-end run; 1.005, 1.049 and 1.007 in `v0.13.json`, every one inside the 0.05 noise
band.

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
