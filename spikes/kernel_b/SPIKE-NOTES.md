# Kernel spike B: trait-bound environment

Maestro leaf `k-spike-b` (task `tsk-mv8ohjj3-zxtk5h`). The design is
`.maestro/specs/0.7/kernel-design-b-traitenv.md`. Per ruling R1 it is a standalone crate here,
not `kernel_v2` in core. The comparison against spike A happens at gate H1.

All numbers are a first pass on a busy shared machine (other agents were compiling).
`w2-spike-gate` re-runs them on a quiet one. No ratios against v0.6 are given:
`docs/benchmarks/v0.6.json` did not exist when this was written.

## What was built

The crate is about 2.3k LOC of `src/` plus the derive crate.

| Piece | Where | Notes |
|---|---|---|
| `Effect<A, E = Never, R = ()>` | `src/kernel.rs` | A step enum: `Ready` (inline, no allocation), `Sync` (one box), `Async` (structural), and `Leaf` (user suspension, interruptible). `Effect` (Send) and `LocalEffect` (!Send) come from one `kernel_impl!` macro_rules template. |
| `Cause<E>` channel | `src/cause.rs` | `Fail(E)`, `Die(Defect)` (keeps the panic payload), `Interrupt(FiberId)`. `Exit<A, E> = Result<A, Cause<E>>`. |
| Typed R | `src/env.rs` | `Has<T>` and `Replace<T>`, both with `on_unimplemented`. `Env` (`()` is deliberately not `Env`). `Service` has a blanket impl. `IntoBind` covers same-`R` binds plus the env-free `()` lift. `caps!` is a trait alias. Spike addition: `impl<T: Service> Has<T> for T`, so a single service is its own env. |
| `#[derive(Env)]` | `derive/src/lib.rs` | Emits `Env`, `Has<F>` and `Replace<F>` per field. A duplicate field type is a derive error. `#[env(flatten)]` works through a derive-emitted `macro_rules! __kernel_b_forward_<Name>` (textual scope, same crate only). `#[env(base(forward(T, ..)))]` covers a generic base. |
| `effect!` / `effect_local!` | `derive/src/lib.rs` | `~Cap` expands to `Has::<Cap>::get(r).clone()`. `~ eff` binds and auto-widens the error (`E1: Into<E>`, or `Never` into any `E` through autoref dispatch). The bound value's type is pinned early through an `ErrTag` read off the effect. |
| Fibers | `src/fiber.rs` | A `FiberState` tree (parent and children), an interrupt flag, a mask depth and a waker. A one-shot `ExitCell` (no `watch`; `join` takes the value, no `Clone` bound). `fork`, `fork_daemon`, `join`, `interrupt` (awaits the exit), `interrupt_fork`. Structured exit: a parent interrupts and awaits its live children. `uninterruptible`, `ensuring` and `acquire_use_release` run finalizers masked. `catch_unwind` around every fiber poll. |
| Interrupt points | `kernel.rs`, `fiber.rs` | Every async `flat_map` continuation. Every pending `Leaf` (innermost first: the leaf is dropped and yields `Interrupt`, so enclosing finalizers still run). Interruptible waits inside `zip_par`/`race`. Pure `Sync` closures and ready binds are not interrupt points. |
| Executors | `src/executor.rs` | An `Executor` trait. `ParkExecutor` (single thread; the `block_on` thread parks when idle) plus a global timer thread. A tokio backend (`tokio::spawn`, `tokio::time::sleep`). `Runtime::{spawn, run}`. `block_on`/`run_blocking` inside a fiber panics with a diagnostic, which turns into a `Die`. There is no `Waker::noop` and no `yield_now` spin anywhere. |
| Concurrency | `src/par.rs` | `zip_par` fails fast and interrupts the sibling. `race` interrupts and awaits the loser. `timeout` and `timeout_fail`. |
| `Layer` | `src/layer.rs` | `from_effect`, `and_then` (feeds the output in as the next env), `merge` (returns a **tuple**), `map`, `identity`. |
| Corpus | `tests/corpus.rs` | All 21 shapes, each with a passing test. |
| Kernel tests | `tests/kernel.rs` | Fork/join, structured children, interrupt latency, finalizers under interrupt, masks, panic payload, `block_on`-in-fiber, CPU under sleep, the tokio runtime, `LocalEffect` with `Rc`, `zip_par` fail-fast, inference probes. |
| Generic-overlay probe | `tests/extension_generic.rs` | The H10 and showcase overlays written generically over the app env. This file is not in the corpus. |
| UI cases | `tests/ui/*.rs` + `.stderr` | 6 trybuild cases, with real rustc output. |
| gen-caps | `harness/gen-caps.sh` | B1 style. The self-test passes. |
| Benches | `benches/gate_b.rs` | The `gates.toml` ids plus `b/interrupt_sleeping_fiber`. Exported to `docs/benchmarks/spike-b.json`. |

## Skipped or not spiked (honest gaps)

- **Cross-crate `flatten` (risk R4).** The forward macro is not `#[macro_export]`, so flattening
  an env defined in another crate **does not work**. Fixing it needs path-qualified exported
  forwarders and a naming scheme. Status: unknown, and currently failing.
- `all(.., Concurrency)`, the Fabric `Scheduler` admission hook (D6), `Runtime::shutdown` (R7),
  `spawn_blocking`, and `fork_in(scope)`.
- `Layer`: no memoization, `fresh` or `scoped` finalizers.
- `LocalEffect` has no fibers (no `fork`), so it runs only through `block_on_local`. Streams (R6)
  were not touched.
- `Executor::now` is unused.
- `ParkExecutor` leaks daemon fibers that are still parked on a timer when `block_on` returns
  (a task → fiber → executor `Arc` cycle until the timer fires).
- The axum `request_caps.rs` was not ported literally. H10 plus W01 stand in for it, as the
  corpus specifies.

## Falsifiers (design §5)

| # | Falsifier | Status | Evidence |
|---|---|---|---|
| R1 | Porting the request overlay plus the showcase needs more than one extra env struct per app, or forward lists exceed about 5 entries | **PASS on the corpus as written; HIT for a generic overlay** | Corpus: 1 extension struct per app (`RequestEnv { #[env(flatten)] app, user }`); `fwd` = **0** (`count-annotations.sh`; W01 and H10 use concrete `flatten`). Written generically over the app env (a reusable auth middleware), the forward list is **2** for H10 and **10** for the showcase-sized env (`tests/extension_generic.rs`, `fwd=12` for the file). The list must also be edited every time the app gains a service. |
| R2 | Send-by-default error noise | Mitigated, partly measured | `tests/ui/not_send_body.stderr` is rustc's standard "future cannot be sent" message. It names the `Rc` and the await, but also points at `kernel_b::box_future` internals. `effect_local!` with an `Rc` held across a bind works. Only 1 of the "3 common traps" was written as a UI case. |
| R3 | Async bind chain more than +25 % vs `phase0_baseline` | **Unknown** (no v0.6 baseline in this session) | `gate/async_bind_64` = 3.47 µs. That includes a fresh `ParkExecutor` and a root fiber per iteration; 0.6 uses `pollster` with no fiber. Spike A's file shows 7.3 µs on the same machine on a different day. |
| — | Interrupt latency above 1 ms | **PASS** | Fork `sleep(10 s)`, then interrupt, then the exit is observed: **p50 1.2 µs, p99 3.3 µs** over 200 samples on the `ParkExecutor` (`interrupt_latency_of_a_sleeping_fiber`). |
| R4 | Cross-crate `flatten` | **Not spiked; fails as built** | See the gaps above. |
| R5 | Compile time (generic `R`) | Measured, see below | `check-time --gen b 16 100`: **0.196 s**, against v06 0.096 s and plain 0.055 s. That is 2.0x v06 on raw medians and 2.9x after subtracting `noop_s`. The 2x falsifier is formally Design A's; this ratio would fail it too. |
| R6 | Streams with `Rc` assumptions | Not spiked | — |
| R7 | Drop-cancel skips async finalizers at shutdown | Not spiked (no `Runtime::shutdown`) | — |
| R8 | Thread-local `CURRENT_FIBER` under work stealing | No failure seen | The tokio multi-thread tests and benches (spawn, sleep, run) pass. The local is set and cleared around each poll. |

## Measured numbers (first pass, noisy)

`docs/benchmarks/spike-b.json` (medians, ns; baseline `spike-b`):

| id | median | note |
|---|---|---|
| `phase0/bench_effect_map_chain` | 10.4 | All `Ready`: applied eagerly, no executor. |
| `phase0/bench_effect_flat_map_chain` | 6.2 | Same. |
| `gate/flat_map_depth_10k` | **53** | **Misleading.** 10k eager `Ready` binds fold to a constant after inlining. The bench measures no runtime. The gate ratio will look like ~10⁴x "better"; treat it as not comparable. |
| `gate/async_bind_64` | 3 469 | 64 `from_async` binds plus fiber setup on a `ParkExecutor`. |
| `gate/spawn_join_10k` | 10 456 699 (10.5 ms) | Tokio multi-thread, root fibers, `exit().await`. |
| `gate/sleep_1k_x_1ms` | 3 384 091 (3.4 ms wall) | |
| `gate/sleep_cpu_1k_x_1ms` | 12 095 773 (12.1 ms CPU) | Real wakers: about 12 µs of CPU per sleeping fiber, with no spin. Test: 1000 × 50 ms sleeps used 8 ms CPU over 53 ms wall. |
| `b/interrupt_sleeping_fiber` | 325 | Not a latency figure: the child is interrupted before its first poll, so this is fork + interrupt-before-start + join through `block_on`. The latency numbers are in the R-table row above. |

Annotation counts: `bash spikes/scripts/count-annotations.sh --json spikes/kernel_b/tests/corpus.rs`

```
"summary": {"handlers": 21, "ported": 21, "unsupported": 0, "missing": 0, "annotated": 0,
            "annotated_pct": 0.0, "total": 0, "index": 0, "fwd": 0}
```

Every shape is 0. The counter does not see what B moves into declarations, which `count-annotations.sh` does not count:

- 32 `Has<..>` bound terms in `fn` signatures.
- **13 `#[derive(Env)]` structs** in the corpus: 3 W01 strata, 1 H10 extension, and 9 that exist only because a multi-service test env needs a named struct.
- 8 `.map_err(AppError::from)?` calls. `?` inside `effect!` goes into `Cause<E>`, so widening a raw service error is manual. Widening on `~ eff` binds is automatic.
- H02 needed a named generic fn with `R: Env` plus a derived env, because the env-free lift requires `R: Env`. A bare single-service env does not qualify.

Check time: `bash spikes/scripts/check-time.sh --gen b 16 100 --json`

```
{"crate": "gen_caps_b_16x100", "runs": [0.196,0.197,0.195], "median_s": 0.196, "noop_s": 0.020}
plain: median_s 0.055 (noop 0.015)   v06: median_s 0.096 (noop 0.036)   rustc 1.99.0-nightly
```

## Inference findings beyond the corpus (`tests/kernel.rs` probes)

1. **A concrete `Env` handler binding a generic helper is E0283.**
   `fn h() -> Effect<_, _, ProbeEnv> { effect!(|r| ~ count()) }` is ambiguous between the
   same-`R` and the env-free `IntoBind` impls, because `ProbeEnv: Env` makes both applicable.
   It needs `count::<ProbeEnv>()`. The design's `(): !Env` coherence trick is the cause. Generic
   (B1) handlers do not hit it, because a rigid `R` without `Env` rules out the `()` impl. Any
   B2-style concrete handler (axum handlers over `AppEnv`) that calls generic helpers does hit
   it. The alternative is to drop the `()` impl and spell env-free lifts `.with_env()`. That
   trades H02's ergonomics for zero ambiguity.
2. A method call on a bound value (`let n = ~ count(); n.pow(1)`) was E0282 until `effect!` was
   changed to pin the value type from the effect type (`ErrTag`). It now compiles with no
   annotation.
3. Inside a non-generic `effect!` with no expected `R` (nested effects in tests), the inner
   effect's `E` often needs a `let x: Effect<_, E, _>` ascription.

## Compile-error readability (real `.stderr`)

| Case | File | Verdict |
|---|---|---|
| Missing capability at the edge | `missing_capability.stderr` | **Good.** "effect environment `AppEnv` does not provide capability `Mailer`", with the field/bound note and "required by a bound in `notify`". It also lists the impls that exist. |
| Missing capability in generic code | `missing_capability_generic.stderr` | Good: the same message on `R`, with the helper's bound shown. Weakness: the span carries no caret (macro span), and `on_unimplemented` replaced rustc's "consider restricting `R`" suggestion. |
| Duplicate, direct | `duplicate_capability.stderr` | **Good.** "`Db` is provided twice in `AppEnv` (fields `primary`, `replica`)" plus newtype help. |
| Duplicate via `flatten` | `duplicate_via_flatten.stderr` | **Poor to fair.** Raw E0119, twice (`Has` and `Replace`). It points at the two derive attributes, never names the field `logger`, and leaks `__kernel_b_forward_BaseEnv`. |
| Wrong provide | `wrong_provide.stderr` | Fair. The message is clear ("has no `MockMailer` slot to replace", and it lists `Replace<Logger>`/`Replace<Mailer>`), but it is 39 lines that include `kernel_impl!` internals. |
| `!Send` body | `not_send_body.stderr` | Fair. Standard rustc, but it points into `box_future`. |

Excerpt (duplicate via flatten):

```
error[E0119]: conflicting implementations of trait `Has<Logger>` for type `AppEnv`
  --> tests/ui/duplicate_via_flatten.rs:6:17
 6 | #[derive(Clone, Env)]
   |                 ^^^ conflicting implementation for `AppEnv`
12 | #[derive(Clone, Env)]
   |                 --- first implementation here
   = note: this error originates in the macro `__kernel_b_forward_BaseEnv` which comes from the
     expansion of the derive macro `Env`
```

## Layer.merge parity loss, concretely (W01)

In Effect.ts, `Layer.merge(Base, Infra)` yields an env of `Config | Logger | Db | ..` with no
user-written type. In spike B the same strata need:

1. Three named structs (`BaseEnv`, `InfraEnv`, `AppEnv`), each derived, with `#[env(flatten)]`
   from each stratum into the next.
2. Per stratum, `Layer::identity().merge(services).map(|(base, (db, sessions, cache))| InfraEnv
   { base, db, sessions, cache })`. `merge` returns `(A, B)`, and the union is named by hand in a
   closure that destructures tuples.
3. No ad-hoc union. Merging a `Layer<Db>` with a `Layer<Mailer>` gives `(Db, Mailer)`, which
   implements neither `Has<Db>` nor `Has<Mailer>` until someone writes a struct for it.
4. Adding a service to the app means touching a struct field, the layer's tuple, and the `map`
   closure. If the request overlay is generic, its forward list changes too (R1).

## Semantics risks found

- **Deep async chains overflow the stack.** 10k left-nested async binds are polled recursively
  (nested boxed futures, no trampoline). That overflows a 2 MiB stack, the tokio worker
  default. The test only passes on a 64 MiB thread. Ready and sync chains do not recurse.
  Spike A's interpreter design may not have this problem; check.
- **Eager `Ready` continuations.** `succeed(x).flat_map(f)` runs `f` at construction time, not
  at run time. An effect that is built but never run still executes `f`, and any side effect in
  `f` happens early. This is the R1 "no-alloc ready bind" taken literally, and it differs
  semantically from Effect.ts laziness.
- `effect!` raw `.await`s on foreign futures are interruptible only because the whole body is a
  `Leaf` that is dropped while pending. Code after the drop point does not run. Use
  `ensuring` or `acquire_use_release`, not code inside the body, for cleanup.
- `provide_service` swaps the slot in place and restores it afterwards. If the future is dropped
  mid-flight the slot is not restored. That is harmless only because the env dies with its
  fiber.
- The `Never` widening and the early value pinning in `effect!` rely on autoref method dispatch
  (`ViaNever`/`ViaInto`) in macro output. It works, but it is clever. A production version
  needs UI tests that pin its error messages.

## Honest assessment

- **Runtime half: solid.** Real wakers, parking `block_on`, structured fibers, masks, finalizers
  under interrupt, and microsecond interrupt latency all work in about 1.5k LOC. Little of that
  is B-specific; most would carry over to A.
- **Typed-R half on the corpus: better than expected.** All 21 shapes compile with 0 counted
  annotations, and the edge errors are good.
- **Where B costs show up:**
  - Declarations: env structs per env combination, bound lists, and manual `map_err`.
  - E0283 for concrete-env handlers calling generic helpers.
  - The `flatten`-duplicate error.
  - No cross-crate `flatten` yet.
  - Compile time about 2x v06 in the first measurement.
  - `Layer.merge` parity: no ad-hoc unions.
- **The R1 falsifier comes down to one question:** must per-request overlays be generic over the
  app env? With one concrete `RequestEnv` per app, B passes (fwd 0). For a reusable middleware
  over any app env, B needs a forward list of every capability downstream handlers read: 10 for
  the showcase. That is above the ~5 threshold, so B fails there.
- Treat the bind and spawn numbers as indicative only until the quiet-machine re-run against
  `v0.6.json`. Exclude `flat_map_depth_10k` from any B-vs-A conclusion.
