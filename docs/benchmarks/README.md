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
  the two of the unary delta, at passing values.
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
| `id_effect_rpc` (feature `postcard`) | `gate_rpc` | `rpc/unary_json`, `rpc/axum_json_handler`, `rpc/unary_postcard`, `rpc/sequential_50`, `rpc/batch_50` |
| `id_effect_rpc` (feature `http3`) | `h3_loss` (no criterion harness; writes criterion estimates) | `h3_loss/h2_p99`, `h3_loss/h3_p99`, `h3_loss/h2_p50`, `h3_loss/h3_p50` |

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
RSS at the end ≤ 1.05 × RSS at minute 1.

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
  Moving the mark to minute 5 or lowering the replay window are owner/coordinator choices (the
  minute-1 mark is the T12 KPI's wording), not changed here.
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
