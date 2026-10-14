# Kernel spike A: notes for gate H1

Design: `.maestro/specs/0.7/kernel-design-a-hlist.md` (typed `R` as an HList with inferred
indices, on an owned fiber runloop). Plus the R1 shared ideas. Leaf `k-spike-a`.
Toolchain for every figure and every `.stderr`: `rustc 1.99.0-nightly (af3d95584 2026-07-09)`
(devenv). Machine: Ryzen 9 7950X, performance governor, **shared with other agents compiling at
the same time**, so every number below is a first pass. `w2-spike-gate` re-runs them on a quiet
machine.

## What was built

About 2.4k lines of kernel src, 0.4k of macros, and 1.1k of tests and benches. The design
estimated about 2.1k in total.

| Piece | File | Status |
|---|---|---|
| `Effect<A, E = Never, R = HNil>`: `Pure` (inline Ready, R1), `Sync` (fused closures, depth ≤ 64), `Op` (erased program) | `src/effect.rs` | built |
| Erased runloop: frame stack, `Seq` continuation vectors, interrupt check at every instruction and before every poll, mask regions, 2048-instruction yield budget, `catch_unwind` → `Die` with the payload kept | `src/runloop.rs` | built |
| `Cause<E>` (`Fail`/`Die`/`Interrupt`) as the only error channel (R1), `Exit`, `ExitKind` | `src/cause.rs` | built (no `Both`/`Then`) |
| Fibers: `FiberContext` tree (parent `Weak`, children map), one-shot `ExitCell` (R1, no `watch`), `fork`/`fork_daemon`/`join`/`interrupt`. Exiting fibers interrupt and await their children. Interrupt wakes the stored real waker. | `src/fiber.rs` | built |
| `Executor` trait, `TokioExecutor` (real timer), `Runtime::{run (async edge), block_on (parks a `Thread` waker), try_block_on → Err(BlockOnInAsync) inside a task, fork}` | `src/executor.rs` | built |
| HList `HNil`/`HCons`, `Selector`/`Plucker`/`Sculptor`/`Embed`/`Concat` with `on_unimplemented` and `do_not_recommend`, `hlist!` | `src/hlist.rs` | built |
| `provide`, `provide_some`, `provide_all`, `provide_env`, `narrow`, `need::<T>()` | `src/effect.rs` | built |
| `effect!` / `effect_local!` / `caps!` (token-level proc macro: `~Cap` reads, `~expr` narrowing binds, `Never` error widening, `caps!` duplicate check) | `macros/src/lib.rs` | built |
| `fork_all` + `Concurrency` (the single D6 choke point), `zip_par` (try-join, sibling interrupt), `timeout`/`timeout_fail` (child fiber raced against the timer) | `src/par.rs` | built; `Bounded` runs in waves, `Inherit` = `Unbounded` |
| `Layer<Out, E, In>`: `succeed`, `from_effect`, `merge`, `provide_merge`, `build` | `src/layer.rs` | built; not memoised, not scoped |
| `LocalEffect` + `effect_local!` (D4) | `src/local.rs` | **thin**, see below |
| `ensuring`, `acquire_use_release`, `uninterruptible`, `uninterruptible_mask` | `src/effect.rs` | built |

### Skipped or simplified (be aware at H1)

- **`LocalEffect` is not a second runloop.** The design asks for one `macro_rules!` template that
  generates both kernels. The spike has a thin async wrapper: inline `Ready`, otherwise one boxed
  `!Send` future. It runs on the calling thread and has no interrupts, no fork, no panic capture,
  and no `provide`. `Executor::spawn_local` always returns `NotLocal` (no `LocalSet`). R1's
  "effect_local!/LocalEffect" is therefore met in API shape only.
- No `TestExecutor` or virtual clock. No `FiberRefs`. No `Admission` wiring (trait declared only).
  No `Layer` memoisation, `fresh` or `scoped`. No `Cause::Both`/`Then`. No `race`.
- `~` inside a nested non-async closure in an `effect!` body is not supported (the bind needs
  `.await`). A `Die`/`Interrupt` of a `~`-bound sub-effect crosses the user's async block as a
  panic that the runloop turns back into the same `Cause`.
- Interrupts are not checked *inside* a fused `Sync` chain (up to 64 steps), only at its boundaries.
- `R` must be `'static`; values cross the erased runloop as `Any`.

## Measured numbers (first pass)

`docs/benchmarks/spike-a.json` comes from `bench-export.sh` over criterion baseline `spike-a`.
`docs/benchmarks/v0.6.json` does not exist yet (b0 leaf), so for orientation I ran the same 0.6
bench files (the W0 `gate_core.rs`/`gate_fiber.rs`, copied into a scratch `git archive v0.6.0`
export under `.tmp/`) in the same session. That is not the official baseline.

These are criterion medians from the final kernel state (`spike-a.json`, sha `03d8a8a`), compared
by `scripts/bench-compare.sh` against the scratch v0.6 export.

| ID | gate | spike A | v0.6 (scratch, same session) | ratio | result |
|---|---|---|---|---|---|
| `phase0/bench_effect_map_chain` | budget ≤ 1.25 | 48.1 ns | 68.7 ns | 0.70 | PASS |
| `phase0/bench_effect_flat_map_chain` | budget ≤ 1.25 | 47.7 ns | 96.5 ns | 0.49 | PASS |
| `gate/flat_map_depth_10k` | budget ≤ 1.25 | 453.6 µs | 490.3 µs | 0.93 | PASS |
| `gate/async_bind_64` | budget ≤ 1.25 | **5.15 µs** | 1.73 µs | **2.98** | **FAIL** |
| `gate/spawn_join_10k` | improve < 1 | 12.8 ms | 6.88 s | 0.002 | PASS (~540×) |
| `gate/sleep_cpu_1k_x_1ms` | improve < 1 | 13.1 ms cpu | 6.78 s cpu | 0.002 | PASS (~520×) |
| `gate/sleep_1k_x_1ms` | report | 3.41 ms | 705 ms | 0.005 | report |
| `kernel_a/need_last_of_16` | (spike-only) | 198 ns | n/a | n/a | report; includes `provide_all` + runloop entry, not a bare lookup |

The `gate/axum_*`, `stream/*` and `coord/*` IDs have no counterpart: the spike has no axum, stream
or channel layer, so bench-compare reports them missing. The 0.6 fiber numbers are seconds
because 0.6 spins a blocking-pool thread per fiber (`run_blocking` + `yield_now`). The
improvement is the R1 waker and runloop design, not the HList.

### Mitigation history for the bind gate (falsifier 1)

| Kernel state | `async_bind_64` | `flat_map_depth_10k` | map/flat_map chains |
|---|---|---|---|
| first cut: box per frame + `Box<dyn Any>` per value + closure-and-pin per `from_async` | 7.36 µs (4.27×) | 638.5 µs (1.30×) | 47.1 / 46.6 ns |
| + SmallBox (inline `()`/word-sized values, safe Rust via `TypeId`) + `Seq` continuation vectors | 5.61 µs (3.26×) | 475.8 µs (0.97×) | 48.0 / 46.6 ns |
| + single-allocation lazy `from_async` future | 5.24 µs (3.04×) | n/a | n/a |
| probe: sync fusion off (`FUSE_MAX = 1`), not kept | 4.47 µs (2.59×) | 472.4 µs | **282.9 / 286.9 ns (4.1× / 3.0×)** |

No fusion setting passes all four bind gates. Fusion is what makes the pure chains beat 0.6, and
it costs async chains about 15%. The floor is the erased runloop itself. Each async bind pays one
continuation box, one boxed future, the fused-closure box at build time, an `Any` downcast,
`catch_unwind` around every step, and two atomic interrupt-flag loads. 0.6 pays roughly three
allocations and polls everything inline under `pollster`. SmallBox only helps values of the
handful of built-in primitive types. A user `struct` of 16 bytes or less is still boxed, because
inline storage of arbitrary types needs `unsafe`, which the workspace forbids.

## Falsifiers (design §5)

| # | Falsifier (design §5) | Status | Evidence |
|---|---|---|---|
| F1 | the bind gate fails after the SmallBox mitigation | **HIT** (first pass) | `async_bind_64` is 2.98× after SmallBox + `Seq` + a single-allocation `from_async` (table above). The other three bind gates pass (0.70 / 0.49 / 0.93). |
| F2 | E0283 reachable in the showcase wiring | **MISS for W01 as specified, but reachable in a natural variant** | W01 wires 10 caps in 3 strata with 0 annotations and no E0283. Merging two layers that both need `ConfigSource` side by side gives E0283 (`tests/ui/duplicate_capability_layer.stderr`). Stacking them with `provide_merge` compiles (`tests/semantics.rs::layers_sharing_an_input_compose_as_strata`). |
| F3 | R3(b): > 20 % of ported handlers need annotations | **MISS** | 1/21 = 4.8 % (H17: 2 turbofish). 2/21 = 9.5 % if H05 used a subset-generic runner. |
| F4 | `cargo check` at 16×100 > 2× the v06 equivalent | **HIT (borderline)** | Quiet re-run: A 0.189–0.194 s vs v06 0.085–0.086 s = **2.2–2.3×** raw. The first noisy pass was 1.72×. Subtracting cargo's no-op overhead gives 3.2×. |
| R6 | `#[diagnostic::do_not_recommend]` cuts the `There<..>` chain | **verified** | `missing_capability.stderr`: one `help` line, no "required for" chain at depth 3. |
| D9 | fibers must improve | PASS | spawn ≈ 540×, sleep CPU ≈ 520× better than 0.6. |

Net: two of the design's four falsifiers are hit on a first, noisy pass (F1 clearly, F4 narrowly).

## Corpus

`tests/corpus.rs`: **21/21 ported, 0 unsupported, 0 missing**. All shapes pass.

`bash spikes/scripts/count-annotations.sh --json spikes/kernel_a/tests/corpus.rs`:

```text
summary: handlers=21 ported=21 unsupported=0 missing=0 annotated=1 annotated_pct=4.8 total=2 index=0 fwd=0
H01-H16, H18-H20, W01: 0 annotations each; H17: turbofish=2 (total 2)
```

Judgement calls (marked `// note:` in the corpus, so the gate can disagree):

- **H05**: the runner ties `R` to the env value it receives (`execute<R, F>(env: R, f: F)`),
  which is exactly 0.6 `execute(State<S>, F: FnOnce(&mut S) -> Effect<_, _, S>)`. With a runner
  generic over a *subset* `R`, the unannotated closure does not compile (E0283;
  `tests/ui/generic_runner_unannotated.stderr`). The fix is one `|_r: &caps!(Counter)|`
  annotation. Had H05 been written that way it would count 1, giving 2/21 = 9.5 %, still under 20 %.
- **H17**: `audit` is generic over `R` as the shape demands. Its callers need
  `~audit::<SignupCaps, _>(..)` (2 turbofish), because `~` narrows from *any* sub-environment and
  so cannot pick the helper's `R` (E0283; `tests/ui/generic_helper_bind.stderr`). The
  alternative of writing `audit` with a concrete `caps!(Logger)` and letting `~` narrow costs
  0 annotations, but that is not "generic over the caller's environment".
- **Signatures with 3 or more caps need a `type` alias.** `caps!(A, B, C)` expands to nested
  `HCons` and trips `clippy::type_complexity` (`-D warnings`). Aliases are declarations and are
  not counted, but every real app with fat handlers will meet this lint.
- H10 uses `.provide(user).provide_env(base)`. `provide_some` (Sculptor) works too, but is not
  needed for one capability.

## Compile errors (trybuild, `tests/ui/*.stderr`)

| Case | Code | Verdict |
|---|---|---|
| missing capability `~Metrics` | E0277, custom headline + label + note; `do_not_recommend` **works** (no `There<..>` chain, one `help` line) | good |
| missing capability through `~count_users()` narrowing | E0277 "environment `HCons<Logger, HNil>` does not contain every capability of `HCons<Db, HNil>`" + one Selector help + a 6-line "required by" | fair |
| duplicate in `caps!(ConfigSource, Db, ConfigSource)` | custom macro error at the 2nd occurrence; **plus** a cascading E0277 on `()` and an unused-import warning | good headline, noisy tail |
| duplicate via `Layer::merge` (both inputs need `ConfigSource`) | **E0283 "type annotations needed … cannot infer `Is1`"**, lists both `Selector` impls, suggests an absurd 4-type turbofish | **poor** (as the design predicted) |
| wrong provide, 1-cap `R` | **E0308 "expected `Greeting`, found `Cache`"**, not the custom message (rustc commits to the only impl) | fair, surprising |
| wrong provide, 2-cap `R` | E0277 custom "`Cache` is not required by this effect, so it cannot be provided", reported **twice** (at depth 1 and 2) | fair |
| `!Send` (`Rc`) across an await in `effect!` | standard "future cannot be sent between threads safely", points at the `Rc` and the await | good |
| subset-generic runner, unannotated closure | two E0283s, suggests `~Counter::</* Type */>` (nonsense) | poor |
| `~` on a helper generic in `R` | three E0283s, suggests `~audit::<R, I>` | poor |

Duplicate-capability stderr, verbatim (`tests/ui/duplicate_capability_layer.stderr`):

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
note: required by a bound in `Layer::<Out, E, In>::merge`
  ...
help: consider specifying the generic arguments
   |
22 |   let _infra = db_layer().merge::<HCons<Mailer, HNil>, HCons<ConfigSource, HNil>, HCons<I, HNil>, HCons<I, HNil>>(mail_layer());
```

Readability verdict: the *missing* and *Send* errors are as good as hand-written diagnostics. The
custom message plus `do_not_recommend` keep them to one headline and one help line at any env
size. Every *inference* failure (duplicates, generic helpers, unanchored closures) is E0283, where
`on_unimplemented` cannot fire. Those errors talk about `Is1`, `There<_>` and impl lists that a
user never wrote.

## Check time (16 caps × 100 handlers)

Each row is `bash spikes/scripts/check-time.sh --gen T 16 100 --json` (median of 3, seconds). All
rows are from one session, run back to back. v06 means
`ID_EFFECT_PATH=.tmp/spike-a-v06/crates/id_effect`, a scratch `git archive v0.6.0` export.

| Template | median_s | noop_s | vs v06 |
|---|---|---|---|
| `a` (this spike, `effect!` + `~`) | 0.194, 0.189 | 0.017 | **2.2–2.3×** |
| `v06` | 0.085, 0.086 | 0.031 | 1.00 |
| `plain` (no kernel) | 0.048 | 0.015 | 0.56 |
| probe: A with `need::<T>()` combinators, no `effect!` (scratch template, not committed) | 0.142 | 0.018 | 1.65× |
| first noisy pass, before the perf commit: `a` / `v06` / `plain` | 0.203 / 0.118 / 0.051 | | 1.72× |

Attribution: about a quarter of A's excess over v06 comes from the per-handler `effect!` async
block. The rest is HList trait resolution (`Selector` per read, `Embed` per `~` and per
`provide_env`) plus closure monomorphisation. Absolute times are small at this size, but the
growth with |R| and the number of handlers was not measured (no 32×1000 run).

## Honest risks

1. **Async bind cost (falsifier hit).** About 3× 0.6 on `async_bind_64`, after the design's
   SmallBox mitigation and two more allocation cuts. Most real handlers are async, so this is the
   number that matters, not the pure chains (which beat 0.6). It is the erased-runloop cost
   (an `Any` value plus a box per frame), not something typed-R itself causes. Design B shares
   the runloop if it follows R1, so compare B's figure before blaming the HList.
2. **E0283 is one plausible refactor away.** W01 as specified wires cleanly. But two infra layers
   that both read `ConfigSource`, combined with `merge`, produce an unreadable E0283. The fix
   (stack them as strata with `provide_merge`; `tests/semantics.rs::layers_sharing_an_input_compose_as_strata`)
   must be taught. Stable Rust has no negative impls, so it cannot be made static.
3. **Generic-R helpers fight `~`.** Narrowing binds and env-polymorphic helpers do not mix
   without a turbofish (H17). Env-polymorphic *constructors* (`succeed`, `sleep`, `from_async`,
   `fiber.join()`) are fine in combinator chains but ambiguous under `~`. The practical rule
   "helpers take a concrete `caps!(..)`; let `~` narrow" works, but it is a rule users must learn.
4. **Compile time** is borderline against the 2× falsifier (see above). Every handler is an async
   block, and every `~`/`provide_env` resolves an `Embed` over the HList.
5. **The `!Send` story is thin** (`LocalEffect` above). A real `macro_rules!` twin runloop is
   extra work that this spike did not price.
6. Error tails print fully qualified paths (`HCons<spike_corpus::Db, HCons<Logger, HNil>>`). That
   is fine at 2 caps and unreadable at 10. rustc never prints the `caps!` alias.
7. Panics are used internally to carry `Die`/`Interrupt` out of a `~` bind through user code. It
   is correct under `panic = "unwind"`. Under `panic = "abort"` a sub-effect defect aborts the
   process.
