# Kernel spike gate: design A (HList typed R) vs design B (trait-bound env)

Leaf `w2-spike-gate` (tsk-mv8ohjj3-w10z06), the input to human gate **H1** (release plan §4 W2,
ruling R1). Written by the gate as a referee. Every number below was re-measured for this report
unless a row says otherwise. Raw outputs and the scripts that produced them are in
[`spike-gate/`](spike-gate/). Section 9 holds the referee's opinion, labelled as such.

| Input | Value |
|---|---|
| v0.6 baseline | `bench/v0.6` = `4894861` (v0.6.0 `68d4b02` + bench hunks of `135ab69`, `a18c4c5`, `2bcd932`), [`v0.6.json`](v0.6.json) |
| Spike A | `spikes/kernel_a` at `090b541` (last commit touching it) |
| Spike B | `spikes/kernel_b` at `ece2333` (last commit touching it) |
| Machine | Ryzen 9 7950X, 32 CPUs, `performance` governor, Linux 6.18.55, `rustc 1.99.0-nightly (af3d95584 2026-07-09)`, criterion 0.8.2 on all three sides |
| Date | 2026-10-15 |

## H1 decision

> **Pending.** The human records the choice here: A / B, plus any conditions. The referee's
> recommendation is in section 9.

## 1. Method

- **Benches.** The same IDs on all three sides. The bench bodies live in
  `crates/{id_effect,id_effect_tokio,id_effect_axum}/benches/*.rs` (v0.6),
  `spikes/kernel_a/benches/gate.rs` and `spikes/kernel_b/benches/gate_b.rs`. Criterion settings
  are identical: the default config, plus `SamplingMode::Flat` with 10 samples for the fiber
  groups on every side. Every bench binary was built before the first round, so no round
  compiled. Each side had its own target dir: `.tmp/target-bench-v06`, `.tmp/target-spikes`.
- **Interleaving.** 5 rounds, each running v0.6, then A, then B, back to back
  ([`rounds.txt`](spike-gate/rounds.txt)). The figure per ID is the **median of the 5 round
  medians**. Per-round values are in [`table.jsonl`](spike-gate/table.jsonl), and the
  median-of-rounds exports are in [`spike-gate/json/`](spike-gate/json/), in the
  `bench-export.sh` format.
- **Quiet check.** Before each round the driver checked for `rustc`, `cargo`, `moon` and
  `cargo-nextest` processes and a 1-minute load average below 8. It polled every 60 s for up to
  30 min. The desktop's own background load is about 5 of 32 CPUs, which is why the load
  threshold is 8. A sampler re-checked every 5 s during the run.
  **Result: 15 of the 16 gate bench sets ran quiet, 1 was contended** (`v06-r2`: one foreign
  build process appeared mid-run). Its `async_bind_64` (1.688 µs) is inside the quiet spread, and
  dropping it moves the v0.6 median by 0.2 %. The baseline run (`v0.6.json`), the probe run and
  every check-time run were quiet. The first baseline attempt waited out a 30-minute busy
  period (another agent's `moon run :check :clippy :test :build`) and was restarted.
- **Gate status (plan's gate command).**
  `bash scripts/bench-compare.sh docs/benchmarks/v0.6.json docs/benchmarks/spike-{a,b}.json`
  **exits 1 for both** (first-pass spike exports, [`gate-command.txt`](spike-gate/gate-command.txt)).
  The median-of-rounds comparison also exits 1 for both ([`compare-a.txt`](spike-gate/compare-a.txt),
  [`compare-b.txt`](spike-gate/compare-b.txt)). In each case the failures are `async_bind_64`
  plus the 6 IDs neither spike implements.

## 2. Benchmarks (median of 5 interleaved rounds)

| ID | Gate | v0.6 | A | B | A / v0.6 | B / v0.6 |
|---|---|---|---|---|---|---|
| `phase0/bench_effect_map_chain` | budget ≤ 1.25 | 67.7 ns | 46.4 ns | 10.8 ns | **0.685** PASS | 0.160 PASS ⚠ NC |
| `phase0/bench_effect_flat_map_chain` | budget ≤ 1.25 | 91.2 ns | 46.9 ns | 5.88 ns | **0.514** PASS | 0.065 PASS ⚠ NC |
| `gate/flat_map_depth_10k` | budget ≤ 1.25 | 491.6 µs | 453.4 µs | 50.1 ns | **0.922** PASS | 0.000 ⚠ **NC** |
| `gate/async_bind_64` | budget ≤ 1.25 | 1.648 µs | 5.185 µs | 3.345 µs | **3.147 FAIL** | **2.030 FAIL** |
| `gate/spawn_join_10k` | improve < 1 | 6.865 s | 15.5 ms | 8.28 ms | 0.0023 PASS | 0.0012 PASS |
| `gate/sleep_cpu_1k_x_1ms` | improve < 1 | 6.823 s cpu | 13.2 ms cpu | 12.9 ms cpu | 0.0019 PASS | 0.0019 PASS |
| `gate/axum_request_oneshot` | improve < 1 | 52.6 µs | — | — | missing (FAIL) | missing (FAIL) |
| `gate/sleep_1k_x_1ms` | report | 689.9 ms | 3.43 ms | 3.33 ms | 0.0050 | 0.0048 |
| `gate/axum_request_caps` | report | 53.3 µs | — | — | missing | missing |
| `stream/pull_map_10k` | report | 17.5 µs | — | — | missing | missing |
| `stream/map_effect_1k` | report | 45.3 ms | — | — | missing | missing |
| `coord/channel_spsc_10k` | report | 1.20 ms | — | — | missing | missing |
| `coord/queue_bounded_10k` | report | 752 µs | — | — | missing | missing |
| `kernel_a/need_last_of_16` | spike-only | — | 195.5 ns | — | | |
| `b/interrupt_sleeping_fiber` | spike-only | — | — | 323.8 ns | | |

Spread across rounds (min to max of the round medians): `async_bind_64` v0.6 1.618 to 1.688 µs,
A 5.119 to 5.224 µs, B 3.299 to 3.378 µs. The ratio is stable to about ±3 %. `spawn_join_10k` is
noisy on both spikes (A 13.6 to 17.7 ms, B 6.8 to 11.8 ms). Both are still about 500 to 1000×
better than v0.6. Codegen sensitivity: the same `async_bind_64` body compiled into a different
binary (the probe in section 3) gives v0.6 2.09 µs, A 5.27 µs and B 3.67 µs, which is
2.52× and 1.75×. Treat the gate ratio as about ±20 % across builds. A and B rank the same either
way.

### Non-comparable (NC) and excluded IDs

- **B `gate/flat_map_depth_10k` (50 ns): not comparable.** B applies `flat_map` on a `Ready`
  effect eagerly at build time (`spikes/kernel_b/src/kernel.rs:196`), so the 10k binds are plain
  function calls and LLVM folds the loop. No runtime and no fiber is measured. With
  `std::hint::black_box` on the value (probe `flat_map_depth_10k_bb`), B takes 1.85 µs for 10k
  binds, A 460 µs, and v0.6 538 µs. B's figure is still build time, not run time.
- **B `phase0/*` chains: semantically not comparable.** These are the same eager `Ready` path
  (`kernel.rs:181`, `:196`), a pure function with no executor. A's figures go through lazy fused
  `Sync` closures and the `block_on` fast path (`kernel_a/src/effect.rs:367-373`, `:394-398`;
  `executor.rs:180-190`), which is the same work 0.6 does under `pollster`. A's ratios are
  comparable.
- **`async_bind_64` is comparable on both,** with one caveat: both spikes run it through
  `block_on`, which builds a fiber (and, for B, a fresh `ParkExecutor`), while 0.6 uses `pollster`
  with no fiber. The scaling probe in section 3 shows this edge accounts for less than 3 % of the
  64-bind figure.
- **Missing on both:** `gate/axum_request_*`, `stream/*` and `coord/*`. Neither spike has an
  axum, stream or channel layer, so `bench-compare.sh` reports them as missing (FAIL by its
  presence rule). The `axum_request_oneshot` *improve* gate therefore cannot be evaluated for
  either design.
- `spawn_join_10k`: A uses `Runtime::fork` and parks, B uses `Runtime::spawn` on a tokio
  multi-thread runtime, and v0.6 uses `run_fork` on `TokioRuntime`. The intent is the same.
  The large improvement comes from the R1 runtime (real wakers instead of 0.6's `run_blocking` +
  `yield_now` spin), not from either typed-R design.

## 3. `gate/async_bind_64`: root cause

**It is not one shared erased runloop.** The two spikes have different runtime representations,
and each misses the budget for its own reasons:

- **A** interprets a type-erased instruction tree on a heap frame stack
  (`spikes/kernel_a/src/runloop.rs:86-131,168-264`).
- **B**, like 0.6, nests one boxed `async` future per combinator
  (`spikes/kernel_b/src/kernel.rs:190-208`; 0.6: `crates/id_effect/src/kernel/effect.rs:304-315`
  at v0.6.0).

**Both pay per bind, not at the edge.** The probe ([`harness/probe`](spike-gate/harness/probe),
raw data in [`probe-r1.json`](spike-gate/json/probe-r1.json)) runs the gate body at n = 1, 16,
64 and 256 binds. The marginal cost is the slope from 16 to 256 binds, the edge cost is n = 1:

| | edge (n = 1) | marginal cost per bind | per-bind ratio vs v0.6 | allocations per bind (1 call) | bytes per bind |
|---|---|---|---|---|---|
| v0.6 | 37 ns | 34.3 ns | 1.00 | 3.0 | 80 |
| A | 122 ns | 69.9 ns | **2.04** | 2.1 | 106 |
| B | 141 ns | 58.9 ns | **1.72** | 4.1 | 199 |

Allocation counts come from a counting global allocator ([`probe-lazy-allocs.txt`](spike-gate/probe-lazy-allocs.txt)).
The extra edge cost over v0.6 is about 85 to 105 ns, which is 1.6 to 2.8 % of the 64-bind
figure. So `block_on`/fiber setup is not the cause.

**A (fewer allocations than 0.6, but about 2× the CPU per bind).** Each async bind goes through
the interpreter:
- A boxed continuation `Cont` (`runloop.rs:79`), called through `guard` = `catch_unwind`
  (`runloop.rs:133-138`, `:156`).
- A boxed `from_async` future (`effect.rs:117-129`).
- A poll inside a second `catch_unwind` with two interrupt-flag loads around it
  (`runloop.rs:176-189`), plus a third at the instruction boundary (`runloop.rs:199`).
- `Frame` push/pop on a `Vec` (`runloop.rs:224-232`, `:153`).
- `AnyBox` erase/unbox for every value. These are `TypeId` ladders: up to 6 `downcast` checks
  each way (`runloop.rs:24-69`).
- Moves of the large `Instr`/`AnyExit` enums.

At build time the sync fusion adds one boxed closure per bind (`effect.rs:335-338`, `:398`).
A's own notes measured that fusion costs async chains about 15 % (FUSE_MAX = 1 probe). The
per-bind cost is therefore CPU in the erased interpreter, not allocation. That matches
SPIKE-NOTES A ("the floor is the erased runloop itself").

**B (0.6's shape plus one box and a TLS check per bind).** Each bind allocates:
- the `structured` closure box and its boxed future (`kernel.rs:198-206`);
- for an env-free `from_async`, a `Leaf` closure box *and* a boxed wrapper future
  (`kernel.rs:144-149`, `:136-141`).

That is 4 allocations against 0.6's 3. Each bind also borrows the thread-local `CURRENT` fiber
`RefCell` for the interrupt point (`kernel.rs:201`, `fiber.rs:117-123`). `block_on` builds a
fresh `ParkExecutor`, a root fiber, a task and a waker per call (`executor.rs:252-263`). That is
the 141 ns edge.

**What a production kernel needs to fit 1.25×.** In the gate binary, 1.25 × 1.648 µs = 2.06 µs
for 64 binds, about 32 ns per bind including the edge. In probe terms the marginal cost must fall
to about 43 ns or less: **A by about 40 %, B by about 27 %**, and each edge must also shrink.
These levers are unmeasured, as is the per-bind cost of narrowing (`~` sub-effects), which the
gate bench does not exercise:

- **B:** store an env-free future directly in the `Leaf` variant. That drops one of the 4
  allocations, which is 0.6 parity on allocations. Read the interrupt flag through the fiber
  instead of a TLS `RefCell` borrow per bind, and reuse the root executor in `block_on`.
  Plausibly 1.2 to 1.4×. But making B lazy and stack-safe (section 8) adds work per bind and
  eats into that margin.
- **A:** catch panics once per fiber poll instead of per step (B does this, `fiber.rs:131-145`).
  Check interrupts once per poll iteration. Add a typed fast path that fuses
  `Op(Future).flat_map(k)` into one boxed state machine, so that same-type async binds skip
  `AnyBox` erasure. Generic inline storage for user structs needs `unsafe`, which the workspace
  forbids (`spikes/Cargo.toml` `unsafe_code = "forbid"`). Closing a 40 % gap with one Q7
  optimisation leaf is uncertain.
- **Both:** the gate's futures are ready on first poll, so it measures pure bind bookkeeping.
  A future that really suspends pays a waker round-trip that dwarfs 30 to 60 ns per bind. That
  is context for an H2 waiver, not a reason to relax the gate.

## 4. Compile errors (real trybuild `.stderr`)

All `.stderr` files are current: `cargo test --test ui` passes for both crates (9/9 A, 6/6 B),
and so does `--test corpus` (21/21 each), as recorded in [`verify-tests.txt`](spike-gate/verify-tests.txt).

| Case | A (`spikes/kernel_a/tests/ui/`) | Rating | B (`spikes/kernel_b/tests/ui/`) | Rating |
|---|---|---|---|---|
| Missing capability (edge) | `missing_capability.stderr` (13 lines) | **good**: custom headline and fix note, one `help`; `do_not_recommend` works; prints the full `HCons<..>` type | `missing_capability.stderr` (20 lines) | **good**: names the env struct, the missing cap and the `notify` bound; adds a derive-origin note |
| Missing capability, nested/generic | `missing_capability_narrowed.stderr` (21 lines) | **fair**: custom headline, then an `Embed<.., HCons<There<_>, HNil>>` "required by" tail | `missing_capability_generic.stderr` (12 lines) | **good**: same message on `R`, shows the helper's bound |
| Duplicate at declaration | `duplicate_capability_caps.stderr` (27 lines) | **fair**: good custom headline, then a cascading E0277 on `()` and an unused-import warning | `duplicate_capability.stderr` (6 lines) | **good**: names both fields, suggests a newtype |
| Duplicate via composition | `duplicate_capability_layer.stderr` (23 lines) | **poor**: E0283 "cannot infer `Is1`", lists `Selector` impls, suggests a 4-type turbofish | `duplicate_via_flatten.stderr` (27 lines) | **poor**: raw E0119 twice, never names the field, leaks `__kernel_b_forward_BaseEnv` |
| Wrong provide | `wrong_provide.stderr` (20 lines); `wrong_provide_multi.stderr` (35 lines) | **fair**: 1-cap is E0308 "expected `Greeting`, found `Cache`" (clear, not custom); 2-cap is custom but reported twice | `wrong_provide.stderr` (39 lines) | **fair**: clear custom message listing the slots, but half of it is `kernel_impl!` internals |
| `!Send` across `.await` | `not_send_body.stderr` (27 lines) | **good**: points at the `Rc` and the await; "required by `from_ctx`" | `not_send_body.stderr` (24 lines) | **good**: identical rustc message; "required by `box_future`" |
| Design-specific inference | `generic_helper_bind.stderr` (73), `generic_runner_unannotated.stderr` (41) | **poor**: E0283 with nonsense turbofish suggestions (`~audit::<R, I>`, `~Counter::</* Type */>`) | concrete-env handler calling a generic helper is E0283 (`tests/kernel.rs` probe, no `.stderr`) | not pinned (reported poor in SPIKE-NOTES B) |

The verbatim `.stderr` for the four plan cases follows, A first, then B.

<details><summary>Missing capability</summary>

A `missing_capability.stderr`:

```text
error[E0277]: capability `Metrics` is not in this effect's environment
  --> tests/ui/missing_capability.rs:11:14
   |
11 |     let m = ~Metrics;
   |              ^^^^^^^ `Metrics` is required here
   |
   = help: the trait `Selector<Metrics, There<There<_>>>` is not implemented for `HCons<spike_corpus::Db, HCons<Logger, HNil>>`
   = note: add `Metrics` to the effect's `caps!(..)`, or remove it with `.provide(..)` / `.provide_env(..)` before running
help: the trait `Selector<T, Here>` is implemented for `HCons<T, Tl>`
  --> src/hlist.rs
   |
   | impl<T, Tl> Selector<T, Here> for HCons<T, Tl> {
   | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

B `missing_capability.stderr`:

```text
error[E0277]: effect environment `AppEnv` does not provide capability `Mailer`
  --> tests/ui/missing_capability.rs:19:11
   |
19 |   let _ = notify().provide(env).block_on();
   |           ^^^^^^^^ `Mailer` is required here
   |
   = note: add a `Mailer` field to your `#[derive(Env)]` struct, or add `Has<Mailer>` to the `R: …` bounds
help: the trait `Has<Mailer>` is not implemented for `AppEnv`
      but trait `Has<spike_corpus::Db>` is implemented for it
  --> tests/ui/missing_capability.rs:12:17
   |
12 | #[derive(Clone, Env)]
   |                 ^^^
   = help: for that trait implementation, expected `spike_corpus::Db`, found `Mailer`
note: required by a bound in `notify`
  --> tests/ui/missing_capability.rs:5:24
   |
 5 | fn notify<R: Has<Db> + Has<Mailer>>() -> Effect<(), AppError, R> {
   |                        ^^^^^^^^^^^ required by this bound in `notify`
   = note: this error originates in the derive macro `Env` (in Nightly builds, run with -Z macro-backtrace for more info)
```

</details>

<details><summary>Duplicate / conflicting capability</summary>

A `duplicate_capability_caps.stderr` (declaration):

```text
error: capability `ConfigSource` appears twice in `caps!(..)`; an environment holds each capability once
 --> tests/ui/duplicate_capability_caps.rs:5:63
  |
5 | fn handler() -> Effect<String, Never, caps!(ConfigSource, Db, ConfigSource)> {
  |                                                               ^^^^^^^^^^^^

warning: unused import: `Db`
 --> tests/ui/duplicate_capability_caps.rs:3:34
  |
3 | use spike_corpus::{ConfigSource, Db};
  |                                  ^^
  |
  = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default

error[E0277]: capability `ConfigSource` is not in this effect's environment
 --> tests/ui/duplicate_capability_caps.rs:6:14
  |
6 |   effect!({ ~ConfigSource.get_string("x").unwrap_or_default() })
  |              ^^^^^^^^^^^^ `ConfigSource` is required here
  |
  = help: the trait `Selector<ConfigSource, _>` is not implemented for `()`
  = note: add `ConfigSource` to the effect's `caps!(..)`, or remove it with `.provide(..)` / `.provide_env(..)` before running
help: the trait `Selector<T, Here>` is implemented for `HCons<T, Tl>`
 --> src/hlist.rs
  |
  | impl<T, Tl> Selector<T, Here> for HCons<T, Tl> {
  | ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
```

A `duplicate_capability_layer.stderr` (composition, excerpt; full file 23 lines):

```text
error[E0283]: type annotations needed
  --> tests/ui/duplicate_capability_layer.rs:22:27
   |
22 |   let _infra = db_layer().merge(mail_layer());
   |                           ^^^^^ cannot infer type of the type parameter `Is1` declared on the method `merge`
   |
   = note: multiple `impl`s satisfying `HCons<ConfigSource, HCons<ConfigSource, HNil>>: Selector<ConfigSource, _>` found in the `kernel_a` crate:
           - impl<T, H, Tl, I> Selector<T, There<I>> for HCons<H, Tl>
             where Tl: Selector<T, I>;
           - impl<T, Tl> Selector<T, Here> for HCons<T, Tl>;
   = note: required for `HCons<ConfigSource, HCons<ConfigSource, HNil>>` to implement `Embed<HCons<ConfigSource, HNil>, HCons<_, HNil>>`
help: consider specifying the generic arguments
   |
22 |   let _infra = db_layer().merge::<HCons<Mailer, HNil>, HCons<ConfigSource, HNil>, HCons<I, HNil>, HCons<I, HNil>>(mail_layer());
```

B `duplicate_capability.stderr` (declaration):

```text
error: `Db` is provided twice in `AppEnv` (fields `primary`, `replica`)
       help: wrap one in a newtype (`struct ReplicaDb(Db);`)
 --> tests/ui/duplicate_capability.rs:8:12
  |
8 |   replica: Db,
  |            ^^
```

B `duplicate_via_flatten.stderr` (composition, first of two identical errors):

```text
error[E0119]: conflicting implementations of trait `Has<Logger>` for type `AppEnv`
  --> tests/ui/duplicate_via_flatten.rs:6:17
   |
 6 | #[derive(Clone, Env)]
   |                 ^^^ conflicting implementation for `AppEnv`
...
12 | #[derive(Clone, Env)]
   |                 ---
   |                 |
   |                 first implementation here
   |                 in this derive macro expansion
   |
   = note: this error originates in the macro `__kernel_b_forward_BaseEnv` which comes from the expansion of the derive macro `Env` (in Nightly builds, run with -Z macro-backtrace for more info)
```

</details>

<details><summary>Wrong provide</summary>

A `wrong_provide.stderr` (1-cap `R`):

```text
error[E0308]: mismatched types
  --> tests/ui/wrong_provide.rs:10:27
   |
10 |   let _ = hello().provide(Cache::default());
   |                   ------- ^^^^^^^^^^^^^^^^ expected `Greeting`, found `Cache`
   |                   |
   |                   arguments to this method are incorrect
   |
help: the return type of this call is `Cache` due to the type of the argument passed
  --> tests/ui/wrong_provide.rs:10:11
   |
10 |   let _ = hello().provide(Cache::default());
   |           ^^^^^^^^^^^^^^^^----------------^
   |                           |
   |                           this argument influences the return type of `provide`
note: method defined here
  --> src/effect.rs
   |
   |   pub fn provide<T, I>(self, t: T) -> Effect<A, E, <R as Plucker<T, I>>::Rest>
   |          ^^^^^^^
```

A `wrong_provide_multi.stderr` (2-cap `R`, first of two errors):

```text
error[E0277]: `Cache` is not required by this effect, so it cannot be provided
  --> tests/ui/wrong_provide_multi.rs:14:19
   |
14 |   let _ = hello().provide(Cache::default());
   |                   ^^^^^^^ this effect's environment `HCons<Logger, HNil>` has no `Cache`
   |
   = help: the trait `Plucker<Cache, There<_>>` is not implemented for `HCons<Logger, HNil>`
   = note: only capabilities in the effect's `R` can be provided; drop this `.provide(..)`
```

B `wrong_provide.stderr` (excerpt; full file 39 lines):

```text
error[E0277]: effect environment `AppEnv` has no `MockMailer` slot to replace
  --> tests/ui/wrong_provide.rs:25:34
   |
25 |   let _ = send().provide_service(MockMailer).provide(env).block_on();
   |                  --------------- ^^^^^^^^^^ `provide_service` needs an existing `MockMailer` slot
   |                  |
   |                  required by a bound introduced by this call
   |
help: the trait `Replace<MockMailer>` is not implemented for `AppEnv`
  --> tests/ui/wrong_provide.rs:15:1
   |
15 | struct AppEnv {
   | ^^^^^^^^^^^^^
   = note: `provide_service` overrides a slot of exactly the slot type; pass a value of the env field's type (newtypes and `Arc<dyn ..>` slots must match exactly)
help: `AppEnv` implements trait `Replace<T>`
  --> tests/ui/wrong_provide.rs:14:17
   |
14 | #[derive(Clone, Env)]
   |                 ^^^
   |                 |
   |                 `Replace<Logger>`
   |                 `Replace<Mailer>`
note: required by a bound in `Effect::<A, E, R>::provide_service`
   … 17 more lines of `kernel_impl!` macro context …
```

</details>

<details><summary>!Send body</summary>

A `not_send_body.stderr`:

```text
error: future cannot be sent between threads safely
  --> tests/ui/not_send_body.rs:9:3
   |
 9 | /   effect!({
10 | |     let local = Rc::new(String::from("not send"));
11 | |     let user = ~Sessions.get("demo").await;
12 | |     local.len() + user.map_or(0, |u| u.len())
13 | |   })
   | |____^ future created by async block is not `Send`
   |
   = help: within `{async block@$DIR/tests/ui/not_send_body.rs:9:3: 13:5}`, the trait `Send` is not implemented for `Rc<String>`
note: future is not `Send` as this value is used across an await
  --> tests/ui/not_send_body.rs:11:38
   |
10 |     let local = Rc::new(String::from("not send"));
   |         ----- has type `Rc<String>` which is not `Send`
11 |     let user = ~Sessions.get("demo").await;
   |                                      ^^^^^ await occurs here, with `local` maybe used later
note: required by a bound in `Effect::<A, E, R>::from_ctx`
   … 7 more lines …
```

B `not_send_body.stderr`:

```text
error: future cannot be sent between threads safely
  --> tests/ui/not_send_body.rs:9:3
   |
 9 | /   effect!(|r| {
10 | |     let cache = Rc::new(String::from("cache"));
11 | |     let user = ~Sessions.get("demo").await;
12 | |     format!("{cache}:{user:?}")
13 | |   })
   | |____^ future created by async block is not `Send`
   |
   = help: within `{async block@$DIR/tests/ui/not_send_body.rs:9:3: 13:5}`, the trait `Send` is not implemented for `Rc<String>`
note: future is not `Send` as this value is used across an await
  --> tests/ui/not_send_body.rs:11:38
   |
10 |     let cache = Rc::new(String::from("cache"));
   |         ----- has type `Rc<String>` which is not `Send`
11 |     let user = ~Sessions.get("demo").await;
   |                                      ^^^^^ await occurs here, with `cache` maybe used later
note: required by a bound in `box_future`
   … 5 more lines …
```

</details>

**Summary.** The everyday errors (missing capability, `!Send`) are good on both. Duplicates are
better on B at the declaration (6 lines, names the fields). Duplicates through composition are
poor on both: E0283 on A, E0119 on B. A has a second class of poor errors, E0283 with nonsense
suggestions, that appears when generic-`R` helpers meet `~` narrowing. B's equivalent (concrete
env plus generic helper, E0283) is not pinned by a UI test.

## 5. Ergonomics

`bash spikes/scripts/count-annotations.sh --json` ([`annotations-a.json`](spike-gate/annotations-a.json),
[`annotations-b.json`](spike-gate/annotations-b.json)). Declaration counts come from
[`declarations.txt`](spike-gate/declarations.txt).

| | A | B |
|---|---|---|
| Corpus coverage | **21/21** ported, 0 unsupported, tests pass | **21/21** ported, 0 unsupported, tests pass |
| Inference annotations (counted) | 1 handler / 21 (4.8 %), 2 turbofish (H17 `~audit::<SignupCaps, _>`) | 0 / 21 |
| `fwd` (forward-list entries) | 0 | 0 in the corpus. A generic overlay probe (`tests/extension_generic.rs`) needs **12**: 2 for H10, 10 for the showcase-sized env |
| Moved into declarations (not counted) | 8 `type X = caps!(..)` aliases (any `R` of 3+ caps trips `clippy::type_complexity`), 29 `caps!(..)` uses | **13 `#[derive(Env)]` structs** (3 W01 strata, 1 H10 extension, 9 that exist only to name a multi-service env), 32 `Has<..>` bound terms, 8 `.map_err(AppError::from)` |
| Corpus size | 596 lines | 683 lines |
| Rules a user must learn | Helpers take a concrete `caps!(..)` and `~` narrows (generic-`R` helpers need a turbofish). Merge layers that share an input as strata (`provide_merge`), not side by side | Every env combination is a named struct. A concrete-env handler calling a generic helper needs `::<Env>`. Widening a service error is manual. `flatten` works within one crate only (R4) |

## 6. Compile time

`spikes/scripts/check-time.sh --gen T 16 100 --runs 7 --json`, two interleaved passes
(plain, v06, a, b, then repeat), on a quiet machine (load about 1). v06 is `ID_EFFECT_PATH` =
the v0.6 export `.tmp/bench-v0.6/crates/id_effect`. Raw data:
[`check-time.jsonl`](spike-gate/check-time.jsonl). A growth point at 32×400 (5 runs) is in
[`check-time-32x400.jsonl`](spike-gate/check-time-32x400.jsonl).

| Template | 16×100 median (pass 1 / 2) | noop | vs v06 raw | vs v06 minus noop | 32×400 median | vs v06 raw |
|---|---|---|---|---|---|---|
| `plain` | 0.046 / 0.047 s | 0.015 | 0.55× | 0.60× | 0.077 s | 0.50× |
| `v06` | 0.085 / 0.085 s | 0.033 | 1.00 | 1.00 | 0.154 s | 1.00 |
| `a` | 0.185 / 0.185 s | 0.018 | **2.18×** | 3.2× | 0.706 s | **4.6×** |
| `b` | 0.170 / 0.173 s | 0.020 | **2.02×** | 2.9× | 0.521 s | **3.4×** |

A's falsifier (above 2× v06 at 16×100) is **hit** at 2.18×. B would also fail that bar at
2.02× raw. It is formally A's falsifier only. A grows faster with size: 4.6× against 3.4× at
32×400.

## 7. Falsifiers (both designs' §5)

| Design | # | Falsifier | Status | Evidence |
|---|---|---|---|---|
| A | F1 / R2 | bind gate fails after the SmallBox mitigation | **HIT** | `async_bind_64` 3.15× (5 quiet rounds); per-bind 2.04× (section 3) |
| A | F2 / R1 | E0283 reachable in the showcase wiring | **MISS as specified** | W01 wires 10 caps with 0 annotations (`kernel_a/tests/corpus.rs:562-569`). The natural variant (merging two layers that both need `ConfigSource`) is E0283 (`duplicate_capability_layer.stderr`) |
| A | F3 / R3(b) | more than 20 % of ported handlers need annotations | **MISS** | 1/21 = 4.8 % |
| A | F4 / R4 | `cargo check` 16×100 above 2× v06 | **HIT** | 2.18× raw, 3.2× net; 4.6× at 32×400 |
| A | R6 | `do_not_recommend` on recursive impls | verified | `missing_capability.stderr` has one `help` line and no `There` chain |
| B | R1 | more than one extra env struct per app, or forward lists above ~5 | **MISS on the corpus, HIT for a generic overlay** | Corpus: 1 extension struct, fwd 0. A reusable overlay generic over the app env needs fwd 10 for the showcase env (`kernel_b/tests/extension_generic.rs:64`). `request_caps.rs` itself was not ported literally |
| B | R3 | async bind more than +25 % | **HIT** | `async_bind_64` 2.03×; per-bind 1.72× |
| B | — | interrupt latency above 1 ms | **MISS** | Re-run: p50 1.16 µs / p99 2.75 µs (debug), p50 0.24 µs / p99 1.98 µs (release) on `ParkExecutor`. A, probed on its tokio executor: p50 9.3 µs / p99 19.8 µs (cross-thread wake, so not like for like) ([`probe-interrupt-latency.txt`](spike-gate/probe-interrupt-latency.txt)) |
| B | R4 | cross-crate `flatten` | **open, fails as built** | The forward macro is not `#[macro_export]` (SPIKE-NOTES B) |
| B | R5 | compile time | measured | 2.02× / 2.9× net at 16×100, 3.4× at 32×400 |
| B | R2, R6, R7, R8 | Send noise / streams / shutdown / TLS under work stealing | R2 mitigated (one UI case); R6, R7 not spiked; R8 no failure seen | SPIKE-NOTES B |

Net: A hits 2 of its 4 (bind cost, compile time). B hits its bind falsifier, and hits R1 under
the generic-overlay reading. That reading is the one that applies to a library-provided axum
overlay, which cannot name the user's env.

## 8. Effect.ts parity (north star D1)

| Property | A | B | Evidence |
|---|---|---|---|
| `Layer.merge` yields the union | **yes**: HList concat, no user type. Not deduplicated: a shared input → E0283; stack such layers as strata | **no**: a tuple `(Out, Out2)`. The union is a hand-written `#[derive(Env)]` struct plus a destructuring `map` | `kernel_a/src/layer.rs:36-55`; `kernel_b/src/layer.rs:42`, `kernel_b/tests/corpus.rs:628-629` |
| `provideService` removes one service from `R` | **yes**: `provide::<T>` (`Plucker`), `provide_some`, `provide_all` | **no**: `provide(env)` eliminates the whole env; `provide_service` replaces a slot and keeps `R`; partial elimination needs `provide_env(S → R)` and a struct | `kernel_a/src/effect.rs:518-588`; `kernel_b/src/kernel.rs:249-305` |
| Calling a smaller-`R` effect | automatic via `~` (`Embed`). Costs index inference (E0283 with generic helpers) and a runtime env rebuild per narrowing (unmeasured) | free via generic `R: Has<..>` bounds (monomorphised). E0283 for concrete env plus generic helper | SPIKE-NOTES A/B |
| Laziness | **lazy**: the probe ran `f` 0 times at build | **eager on `Ready`**: `succeed(x).flat_map(f)` and `.map(g)` each ran 1 time at build (**verified**). v0.6: 0 | [`probe-lazy-allocs.txt`](spike-gate/probe-lazy-allocs.txt); `kernel_b/src/kernel.rs:181,196` vs `kernel_a/src/effect.rs:367-372,394` |
| Deep left-nested chains | **stack-safe**: 100k async and 100k sync binds on a 2 MiB thread in release; 10k (the largest tested) in debug | **not stack-safe for async**: release overflows at 100k on 2 MiB *and* 8 MiB (OK on 64 MiB); debug overflows at 10k on 2 MiB (SPIKE-NOTES B's claim, **reproduced in debug only**; release handles 10k). Sync chains are fine (eager). v0.6 overflows at 100k (release) and 10k (debug) on 2 MiB | [`probe-stack.txt`](spike-gate/probe-stack.txt), [`probe-stack-debug.txt`](spike-gate/probe-stack-debug.txt); A: heap frame stack `runloop.rs:115-165`, `FUSE_MAX` `effect.rs:21`; B: nested polls `kernel.rs:198-206` |
| `LocalEffect` / `!Send` | thin wrapper: no interrupts, fork, panic capture or provide | `kernel_impl!` twin of the same combinators, no fork | SPIKE-NOTES A/B |

**Is B's eagerness inherent?** Not to the trait-bound env. It comes from the R1 "inline `Ready`"
idea taken literally in an erased step enum. A has the same `Pure` variant but defers the
continuation into one boxed `Sync` closure (`effect.rs:394`). B can do the same, at one box per
ready bind. Being lazy without that box would need the combinator in the effect's type
(futures-style), which neither design has. The fix costs B most of its `phase0` advantage, down
to roughly A's 46 ns level, still well within budget. B's `flat_map_depth_10k` would then become
measurable. Stack safety for B's async chains needs a trampoline or a frame stack, which is A's
design. That adds per-bind work against the async budget.

## 9. Decision inputs

1. **Runtime half: a wash, and reusable.** Both meet R1: real wakers, one-shot exit, `Cause<E>`,
   structured fibers, µs interrupt latency, about 500 to 1000× better spawn and sleep CPU than
   0.6. B's runtime pieces are further along (the `kernel_impl!` twin `LocalEffect`, the
   `ParkExecutor`), and either design can adopt them.
2. **Async bind: both fail D9.** A 3.15×, B 2.03×. The cause is per-bind cost in each design's
   own representation (section 3), not the shared edge. B needs about −27 % per bind, A about
   −40 %. B's path to budget is shorter, but making B lazy and stack-safe pushes it back toward
   A's representation.
3. **Typed-R semantics vs Effect.ts: A wins clearly.** Union layers, provide-one, automatic
   narrowing, lazy, stack-safe. B gives up `Layer.merge` unions and needs a named struct per env
   combination (13 in the corpus). It also needs forward lists for generic overlays (10 for the
   showcase), and its cross-crate `flatten` does not work yet. Those last two hit exactly the
   library-overlay case (`id_effect_axum` over a user's `AppEnv`).
4. **Day-to-day friction: B is slightly better.** 0 counted annotations against 2. Better
   duplicate errors at the declaration. No HList types in messages. A's E0283 class (generic
   helpers under `~`, duplicate inputs in `merge`) is intrinsic to HLists on stable Rust and has
   to be taught.
5. **Compile time: B better.** Both are about 2× v0.6 at 16×100 (A 2.18× hits its falsifier;
   B 2.02×). A grows faster (4.6× vs 3.4× at 32×400).

**Referee's opinion (not a measurement):** choose **A**, with **moderate confidence (about
60 %)**. D1 makes Effect.ts composition the north star. A's advantages there (union layers,
provide-one, laziness, stack safety) are structural. B's are mostly fixable engineering (its
bind cost) or costs that A also pays (compile time, about 2×). B's typed-R gaps (no unions,
per-env structs, forward lists for library overlays, cross-crate `flatten`) are in the design
itself. What would flip this to B: the human weighting runtime and compile cost plus
Rust-idiomatic diagnostics over Effect.ts parity, and accepting that overlays shipped by the
library take a forward list or a different extension mechanism.

**What each choice implies for D9:**

- **A:**
  - `async_bind_64` needs about a 40 % per-bind cut. The one Q7 optimisation leaf (fewer
    `catch_unwind`/interrupt checks, a typed async-bind fast path) is unlikely to reach
    1.25× alone, so plan for an **H2 waiver on `gate/async_bind_64`**.
  - The other three budget gates pass with margin (0.69 / 0.51 / 0.92). The fiber improve gates
    pass.
  - Track compile time as a non-D9 risk with a budget (4.6× at 32×400).
- **B:**
  - The Q7 leaf (direct `Leaf` future, no TLS check per bind) could plausibly reach about 1.2
    to 1.4×. An H2 waiver is still probable, and the gap is smaller.
  - The laziness fix and a trampoline must land first. They move `phase0` toward 0.5 to 0.7,
    still a PASS, make `flat_map_depth_10k` measurable, and add per-bind work.
- **Either:** `axum_request_oneshot` (improve), `stream/*` and `coord/*` are unmeasured until
  `k-rebuild` and the migration leaves land, so the D9 picture is incomplete until
  `r-bench-report`.

## 10. Files

| Path | Content |
|---|---|
| `spike-gate/rounds.txt` | one line per bench set: start/end, quiet check, busy samples, quiet/contended |
| `spike-gate/raw/*.criterion.txt`, `*.busy` | criterion stdout per round; process samples every 5 s |
| `spike-gate/json/{v06,a,b}-r{1..5}.json` | per-round exports (`bench-export.sh` format) |
| `spike-gate/json/{v06,a,b}-median.json` | median-of-rounds exports (`round_medians` kept per ID) |
| `spike-gate/table.jsonl`, `compare-{a,b}.txt`, `gate-command.txt` | the section 2 table source; `bench-compare.sh` output |
| `spike-gate/json/probe-r1.json`, `probe-*.txt` | scaling, allocation, laziness, stack and interrupt-latency probes |
| `spike-gate/annotations-*.json`, `declarations.txt` | section 5 |
| `spike-gate/check-time*.jsonl`, `check-time.txt` | section 6 |
| `spike-gate/verify-tests.txt` | UI and corpus test re-runs |
| `spike-gate/harness/` | the scripts and the probe crate. Copy `harness/` to `.tmp/spike-gate/` to re-run; the probe crate's path dependencies assume `.tmp/spike-gate/probe` |
