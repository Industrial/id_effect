# ADR 0011 — Fiber runtime, Send by default, typed R, side-by-side transition

## Status

Accepted (2026-10-15). Mission `id-effect-runtime-0-7`, task `w2-adr-0011`. Records
human gate H1 ([spike report, "H1 decision"](../benchmarks/SPIKE-REPORT.md#h1-decision)):
**design A** (HList typed R), specified in `.maestro/specs/0.7/kernel-design-a-hlist.md`.
Partly supersedes ADRs 0002–0005 (see [ADR 0003 Amendment 1](0003-capability-set-and-provider-lifecycle.md#amendment-1-2026-10-15-for-07))
and amends ADR 0007 Phase C (see [ADR 0007 Amendment 1](0007-compute-fabric.md#amendment-1-2026-10-15-phase-c-for-07)).

## Context

The 0.6 kernel cannot carry the 0.7 brief (decisions D3 to D6):

- `Effect` is a boxed op whose future is `dyn Future` without `Send`
  (`crates/id_effect/src/kernel/effect.rs:73`), and that future borrows `&mut R` for its
  whole poll lifetime (`effect.rs:52-56`). A spawned `'static + Send` fiber that owns `R`
  and a future borrowing it would be self-referential, which `#![forbid(unsafe_code)]`
  rules out.
- So fibers are driven by `run_blocking` on tokio's blocking pool
  (`crates/id_effect_tokio/src/lib.rs:179-195`), and `run_blocking` busy-polls with
  `Waker::noop` plus `thread::yield_now` (`crates/id_effect/src/runtime/execute.rs:72-77`).
  0.6 needs 6.8 s of CPU for 1k concurrent 1 ms sleeps (`gate/sleep_cpu_1k_x_1ms`,
  [SPIKE-REPORT §2](../benchmarks/SPIKE-REPORT.md#2-benchmarks-median-of-5-interleaved-rounds)).
- Interruption only fails a `Deferred` (`concurrency/fiber_handle.rs:92-94`); the job keeps
  running, the `parent` argument is ignored (`runtime/rt.rs:34-45`), and fiber `A` and `E`
  must be `Clone + Send + Sync` (`rt.rs:27-28`).
- `R` is not checked. `Needs<T>` and `HasCap<K>` have blanket impls for any
  `Deref<Target = Env>` (`capability/needs.rs:40-49`, `capability/set.rs:183`), and a
  missing capability panics at run time (`capability/env.rs:146-155`,
  `capability/cap_bind.rs:122-123`).

Two kernel designs were spiked in parallel (ruling R1): A, typed R as an HList with
index inference on an owned instruction runloop, and B, a trait-bound env struct. Both
adopted the shared ideas: an inline `Ready` variant, `Cause<E>` as the internal error
channel, a one-shot completion slot and `effect_local!`/`LocalEffect`. The gate report
([`SPIKE-REPORT.md`](../benchmarks/SPIKE-REPORT.md)) measured both against the v0.6
baseline.

## Decision

### Fiber runtime on an `Executor` trait (D3)

id_effect runs its own fiber interpreter. Each fiber is one runloop future that drives an
instruction tree with an explicit, heap-allocated frame stack (stack-safe at depth). The
runloop is `Send + 'static` and runs on an executor:

```rust
pub trait Executor: Send + Sync + 'static {
  fn spawn(&self, fut: BoxFuture<'static, ()>);
  fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal>;
  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>);
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()>;   // a real timer
  fn now(&self) -> Instant;
}
```

- **tokio is the default backend and stays mandatory** (ruling R6, recorded in
  [ADR 0010 Amendment 3](0010-crate-tiers.md#amendment-3-2026-10-15-for-07)).
  `TokioExecutor` wraps a `tokio::runtime::Handle`. `TestExecutor` (virtual clock,
  deterministic queue) is the test backend. The trait keeps another backend possible
  later without a cfg matrix now.
- **Real wakers everywhere.** No `Waker::noop`, no `yield_now` spin, no `thread::sleep`.
  The K exit contract checks `rg -n 'Waker::noop|yield_now' crates/id_effect/src/next` = 0.
- **Edges.** `Runtime::run(eff).await` is the async edge. `block_on(eff)` parks the thread
  on a real `Waker` (`std::task::Wake` + `Thread::unpark`). Called on a tokio worker,
  `block_on` returns `Err(BlockOnInAsync)` instead of deadlocking or spinning. This removes
  the class of bug fixed by hand in `w1-request-resolver`.
- **Completion.** A fiber completes a one-shot exit cell. `FiberHandle::join` has no
  `Clone + Sync` bound on `A` or `E`; that bound came from the 0.6 `watch`-based
  `Deferred`.

### Send by default, `LocalEffect` opt-in (D4)

`Effect<A, E, R>` is `Send` whenever `A` and `E` are, because every boxed node carries
`+ Send`. The env is borrowed only synchronously: a `Sync` step gets `&R` and returns;
an `Async` step gets `&Ctx<R>`, clones what it needs and returns a `'static` future. Nothing
is self-referential.

`LocalEffect<A, E, R>` is the `!Send` twin for `Rc`/`RefCell` code. Both kernels are
generated from one template so their combinators cannot drift. `Effect → LocalEffect` is
free (`.local()`); `LocalEffect → Effect` goes only through `run_on_local`, which runs the
effect on a `spawn_local` worker and returns a `Send` handle. The spike's `LocalEffect`
was a thin wrapper without interrupts, fork, panic capture or `provide`
(SPIKE-REPORT §8); the rebuild must give it the full combinator set.

### Typed R as an HList (D5)

```rust
pub struct HNil;  pub struct HCons<H, T>(pub Arc<H>, pub T);
pub trait Selector<T, I> { fn get(&self) -> &Arc<T>; }      // `~T` / `need::<T>()`
pub trait Plucker<T, I>  { type Rest; /* … */ }              // `provide` drops one element
pub trait Embed<Sub, Is> { fn project(&self) -> Sub; }       // `~` narrows to any subset
pub trait Sculptor<Sub, Is> { type Rest; /* … */ }           // `provide_some`, layers
```

- `caps!(Db, Logger)` is `HCons<Db, HCons<Logger, HNil>>`, built recursively with no arity
  limit. Index types (`Here`, `There<I>`) are inferred.
- **A missing capability is a compile error.** `Selector`, `Plucker` and `Embed` carry
  `#[diagnostic::on_unimplemented]`, and the recursive `There` impls carry
  `#[diagnostic::do_not_recommend]`, so the message has one `help` line and no `There`
  chain (spike UI test `missing_capability.stderr`, falsifier R6 verified).
- `provide::<T>` removes exactly one element from `R`; `provide_some` and `provide_layer`
  remove a subset. Only `Effect<_, _, HNil>` is runnable, so there is no run-time
  `verify`.
- `Layer<Out, E, In>` values compose: `merge` yields the union of outputs (HList concat),
  `provide_to` feeds one layer into another. `fresh` and `scoped` are added in W4.
- `Env` survives as `DynEnv`, an explicit escape hatch whose lookups are fallible only
  (`try_need -> Result<_, CapabilityError>`). `need` can never panic.

The capability-model consequences for ADRs 0002–0005 are in
[ADR 0003 Amendment 1](0003-capability-set-and-provider-lifecycle.md#amendment-1-2026-10-15-for-07).

### Interruption model

Interruption follows Effect.ts semantics:

- **Interrupt points.** The runloop checks the fiber's interrupt flag before each
  instruction, before polling an `Async` future, and whenever it is woken.
  `FiberHandle::interrupt` requests interruption and awaits the fiber's exit.
- **Stopping work.** Interrupting drops the in-flight future (cancellation), unwinds the
  frame stack, and runs `ensuring` and `acquire_release` finalizers with interruption
  masked.
- **Masks.** `uninterruptible(eff)`, `interruptible(eff)` and
  `uninterruptible_mask(|restore| …)` push and pop a per-runloop mask. A request that
  arrives under a mask stays pending and is honoured when the region exits.
- **Fiber trees.** `fork` registers the child under the current fiber. When a fiber exits
  (success, failure or interrupt) it interrupts and awaits its live children before it
  completes its exit cell. `fork_daemon` attaches to the global root scope instead;
  `fork_in(scope)` ties the child to an explicit `Scope`.
- **Parallel combinators** (`zip_par`, `all`, `for_each_par`, the `effect!` auto-parallel
  `join_binds*`) go through one `fork_all(children, Concurrency)`. On the first failure it
  interrupts and awaits the siblings, then fails. See
  [ADR 0007 Amendment 1](0007-compute-fabric.md#amendment-1-2026-10-15-phase-c-for-07) for
  the Fabric hook.

### Panics become `Defect`

A panic in a fiber body, a finalizer or a runtime edge is caught and surfaces as
`Cause::Die(Defect)` (`crates/id_effect/src/failure/cause.rs:150`), with the panic payload
kept and downcastable (`w1-defect-type`). A panicking finalizer yields `Then(_, Die)`.
Per H1 condition 1, `catch_unwind` sits at fiber-poll and edge boundaries, not around
every step.

### Side-by-side transition

The new kernel does not replace the old one in place (ruling R2; release plan §3a and §3b):

1. **Landing site.** `k-rebuild` adds `#[doc(hidden)] pub mod next` in `crates/id_effect`
   (`crates/id_effect/src/next/**`), always compiled and not behind a feature. It is not a
   separate crate: that crate would need core's `Cause`, `Defect`, `Clock`, `Duration` and
   `Schema` (a dependency cycle or duplication), and the orphan rule would block impls on
   core types. Kernel-independent modules (`failure`, `foundation`, `schema`,
   `scheduling::{clock,duration,datetime}`, `match_`) stay at the root and serve both
   kernels. Temporary macros (`effect_next!`, `caps_next!`, …) target `next`.
2. **The old kernel is untouched.** Outside `next/`, `crates/id_effect/src` changes only by
   the `pub mod next;` line until retirement (K exit contract item 7).
3. **Switch-over in publish-level order.** Each crate switches only after every workspace
   dependency it has has switched, so a migrated crate never calls an old-kernel API.
   Unmigrated dependents (crates, labs, examples) call a switched crate through
   `id_effect::next::compat`: `IntoV1` turns `next::Effect<A, E, HNil>` into a v1 `Effect`,
   and `IntoNext` turns a v1 `Effect` into a `next::LocalEffect` with an explicit layer at
   the boundary. Adapters are deleted when their dependent switches.
4. **Retirement.** `k-retire-v1` checks `rg -n 'next::compat' crates labs examples` = 0,
   then deletes the v1 kernel and the bridge and promotes `next/*` to the root.
   `k-retire-sweep` does the mechanical rename (`id_effect::next::` → `id_effect::`,
   `effect_next!` → `effect!`) and deletes the `next` alias and the temporary macro names.

The workspace (`cargo check --workspace`, `scripts/ci-local.sh full`, the labs tests) is
green at every commit of the transition.

### Conditions carried into `k-rebuild` (H1)

1. A dedicated runloop-optimisation leaf targets `gate/async_bind_64` ≤ 1.25× v0.6: move
   `catch_unwind` to fiber and edge boundaries, amortise interrupt checks, fuse sync steps,
   and avoid per-value erasure where the type is statically known.
2. `Layer::merge` dedupes or rejects duplicate capabilities with a custom diagnostic
   instead of reaching E0283.
3. Generic-helper narrowing (corpus handler H17) gets a documented pattern or a macro.

## Evidence (spike gate, median of 5 interleaved rounds)

| ID | Gate | v0.6 | A | A / v0.6 |
|---|---|---|---|---|
| `phase0/bench_effect_map_chain` | ≤ 1.25 | 67.7 ns | 46.4 ns | 0.685 PASS |
| `phase0/bench_effect_flat_map_chain` | ≤ 1.25 | 91.2 ns | 46.9 ns | 0.514 PASS |
| `gate/flat_map_depth_10k` | ≤ 1.25 | 491.6 µs | 453.4 µs | 0.922 PASS |
| `gate/async_bind_64` | ≤ 1.25 | 1.648 µs | 5.185 µs | **3.147 FAIL** |
| `gate/spawn_join_10k` | < 1 | 6.865 s | 15.5 ms | 0.0023 PASS |
| `gate/sleep_cpu_1k_x_1ms` | < 1 | 6.823 s cpu | 13.2 ms cpu | 0.0019 PASS |

Other measurements behind the decision (SPIKE-REPORT §3 and §5 to §8):

- Per-bind cost on async chains is 2.04× v0.6 (69.9 ns against 34.3 ns) with fewer
  allocations (2.1 against 3.0). The cost is CPU in the erased interpreter, not the edge.
- The 21-handler corpus ports completely; 1 handler (4.8 %) needs an annotation, the
  H17 turbofish. The showcase-sized wiring (10 caps) needs none.
- `cargo check` at 16 caps × 100 handlers: 2.18× v0.6 (4.6× at 32 × 400).
- Lazy (0 builds of `f` at construction) and stack-safe at 100k async and sync binds on a
  2 MiB thread in release.

## Alternatives considered

- **Design B, trait-bound env** (`spikes/kernel_b`, spec
  `.maestro/specs/0.7/kernel-design-b-traitenv.md`). Rejected at H1, with the referee's
  moderate confidence (about 60 %) for A. B was better on several measured axes: async bind
  2.03× instead of 3.15×, `cargo check` 2.02× instead of 2.18× (3.4× instead of 4.6× at
  32 × 400), 0 counted annotations instead of 2, and no HList types in error messages. It
  lost on semantics that matter for Effect.ts parity (D1):
  - `Layer::merge` yields a tuple, not a union; every env combination needs a named
    `#[derive(Env)]` struct (13 in the corpus).
  - `provide` eliminates the whole env; there is no provide-one-service.
  - It is eager on `Ready`: `succeed(x).flat_map(f)` ran `f` at build time, so its
    `phase0/*` and `flat_map_depth_10k` figures were not comparable.
  - It is not stack-safe for async chains: release overflows at 100k binds on 2 MiB and
    8 MiB stacks.
  - A library overlay generic over the user's env (the `id_effect_axum` case) needs a
    forward list of 10 entries for the showcase env, and the cross-crate `flatten` macro
    did not work as built.

  Fixing laziness and stack safety would cost B the per-bind box A already pays, moving
  it toward A's representation, while its typed-R gaps are in the design itself.
- **Quarantine transition.** Replace the kernel in place, quarantine the crates that do not
  compile yet and bring them back one by one (`w3-quarantine`, `m-quarantine-exit` in the
  first plan draft). Rejected by ruling R2: the workspace would be red or partial for most
  of W3–W5, and a failed rebuild would leave nothing working. Side by side keeps 0.6
  behaviour intact until `k-retire-v1`, at the price of both kernels coexisting for a while.
- **Tokio-native (no own interpreter).** Make `Effect` a thin wrapper over `Send` futures
  and use `tokio::spawn`, `JoinHandle::abort` and `tokio::select!` directly. Rejected:
  abort drops a task at its next await without running effect finalizers under a mask,
  there is no parent/child fiber tree or interrupt mask to build Effect.ts semantics on,
  and deep effect chains would nest futures (the 0.6 stack-overflow shape). Tokio remains
  the executor under the interpreter.
- **Optional tokio.** Rejected by ruling R6; see ADR 0010 Amendment 3.

## Consequences

- **Breaking, once.** `Effect<A, E, R>` changes signature a single time for D3–D5.
  `effect!` bodies take `r: &R` instead of `&mut R`, must be `Send` across `~` (no
  `Rc`/`RefCell`), and generic helpers grow index parameters (`R: Selector<T, I>`).
  `docs/MIGRATION-0.7.md` covers every change.
- **Async-bind budget risk.** `gate/async_bind_64` is 3.15× against a 1.25× budget, and a
  40 % per-bind cut is needed. The optimisation leaf (H1 condition 1) may not reach it.
  H2, the D9 waiver, is **not** granted now: it is decided at `r-bench-report` only if
  that leaf misses 1.25×. The gate futures are ready on first poll, so the figure is pure
  bind bookkeeping; a future that really suspends pays a waker round-trip far larger than
  the per-bind gap.
- **Compile time.** About 2.2× v0.6 at 16 × 100 (A's own falsifier, above 2×, is hit),
  growing to 4.6× at 32 × 400. Tracked as a non-D9 risk. While both kernels coexist
  (W3–W5), build time and code size grow further; this is accepted as temporary.
- **E0283 risk.** `on_unimplemented` does not fire on ambiguity, so a duplicated
  capability in `R` gives a bare "type annotations needed" (spike UI test
  `duplicate_capability_layer.stderr`: merging two layers that both need the same input).
  Mitigations: H1 condition 2 (`Layer::merge` dedupes or rejects with a custom
  diagnostic), a `caps!` duplicate check at macro time, layering such layers as strata
  (`provide_merge`) so the flat app env is built once at the edge, and the H17 pattern
  for generic helpers (H1 condition 3).
- **Runtime wins.** Spawn/join and sleep CPU improve by about 400× and 500× over v0.6.
  Interrupts actually stop work. The `run_blocking(..).expect` noise around pure
  constructors goes away, because `Deferred::new()` and similar become plain functions.
- `spikes/` is deleted by `k-rebuild` once the chosen kernel lands.
