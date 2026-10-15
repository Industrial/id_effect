# Kernel Design A — typed R via HList + index inference, on an owned fiber runloop

Status: DRAFT for human review (competes with Design B, trait-bound env). Scope: D3 + D4 + D5 + D6 hook,
so the `Effect` signature changes exactly once. All paths are relative to the
`release/0.7.0` worktree. Nothing here is implemented.

## 0. Why the current kernel cannot carry D3/D4/D5

- `Effect` is `Box<dyn EffectOp<A,E,R>>` (`crates/id_effect/src/kernel/effect.rs:215`); each `map`/`flat_map`
  allocates a new op plus a `Box::pin(async …)` (`effect.rs:272-283`, `304-315`), and the inner `run` boxes
  again, so polling a depth-n chain re-enters n nested futures.
- The env is borrowed `&'a mut R` for the whole poll lifetime (`effect.rs:52-56`, `effect.rs:261`). A spawned,
  `'static + Send` fiber future that owns `R` *and* a future borrowing it is self-referential, which
  `#![forbid(unsafe_code)]` rules out. That borrow, more than `dyn Future` without `Send` (`effect.rs:73`), is why fibers
  run through `run_blocking` on blocking threads (`crates/id_effect_tokio/src/lib.rs:191-195`).
- R is not checked at all. Both `Needs<T>` (`capability/needs.rs:40-49`) and `HasCap<K>`
  (`capability/set.rs:183`) have blanket impls for any `Deref<Target = Env>`, and `~sub()` narrowing does a
  runtime `verify` and then panics (`capability/cap_bind.rs:120-130`).

## 1. Core signatures

### 1.1 Representation: instruction enum + sync fusion (chosen)

```rust
pub struct Effect<A, E = Never, R = HNil> { node: Node<A, E, R> }       // Send iff A,E: Send (auto)
enum Node<A, E, R> {
  Pure(Result<A, E>),                                                  // inline, 0 alloc
  Sync(Box<dyn FnOnce(&R) -> Result<A, E> + Send>),                    // fused map/flat_map chains
  Async(Box<dyn FnOnce(&Ctx<R>) -> BoxFuture<'static, Result<A, E>> + Send>),
  Op(Box<dyn Instr<A, E, R>>),   // FlatMap/Fold/Fork/Provide/Region/Ensuring/Yield: erased runloop frames
}
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub struct Ctx<R> { env: Arc<R>, fiber: Arc<FiberContext> }           // clone = 2 refcount bumps
```

Key move: **the env is borrowed only synchronously.** `Sync` gets `&R` and returns. `Async` gets `&Ctx<R>`, clones
what it needs, and returns a `'static` future. Nothing is self-referential, so the runloop can be `Send + 'static`
and run on any `Executor`. User `async` blocks produced by `effect!` own a `Ctx` clone. A borrow *inside* an
`async` block is fine, because the compiler-generated state machine is allowed to borrow its own locals.

Alternatives considered:

| Option | Allocs per `map` | Poll depth | Interrupt points | Verdict |
|---|---|---|---|---|
| 0.6 boxed op + `Box::pin` | 2–3 (`effect.rs:236-250`, `278`) | O(n) nested | only at futures | baseline |
| Boxed op tree, Send | 2–3 | O(n) | only at futures | no gain |
| Fused future (`Effect<A,E,R,F: Future>`) | 0 | 1 | none (opaque) | type explosion; no recursion; no uniform type for `Vec<Effect>`; can't host interrupts or fiber trees |
| **Instr enum + sync fusion** | 0 (`Pure`), 1 (`Sync` compose / `Op` frame) | O(1) per step, stack-safe trampoline | every instruction boundary | **chosen** |

Cost argument against the ≤25% budget (D9): `Pure(..).map(f)` becomes `Sync(Box(f∘ready))`. That is one allocation
where 0.6 needs two or more. A `Sync`→`Sync` `map` or `flat_map` composes closures (one allocation, no future). Only
`Async` and `Op` cross into the runloop, which keeps a `Vec<Box<dyn Frame>>` continuation stack, so each async
`flat_map` frame costs one box plus a `Box<dyn Any + Send>` for the intermediate value. The phase0 benches
(`crates/id_effect/benches/phase0_baseline.rs:7-34`) are all-pure chains, so this design should be at or below
baseline. The real risk is the `Any` boxing on async-heavy chains (§5, R2).

### 1.2 LocalEffect (D4)

`LocalEffect<A,E,R>` uses the same `Node`, but its boxes drop `+ Send` and `BoxFuture` is replaced by
`LocalBoxFuture`. Both kernels come from one `macro_rules!` template in `kernel/node.rs` so they cannot drift.
- `Effect → LocalEffect`: free (`.local()`).
- `LocalEffect → Effect`: only via `run_on_local(exec)`, which spawns onto a `spawn_local` worker and returns a
  `Send` `FiberHandle`.
- `Rc`-based `Stream` (the reason for `!Send`, `effect.rs:70`) moves to `Arc`, or becomes `LocalStream`.

### 1.3 Executor, Fiber, interrupts

```rust
pub trait Executor: Send + Sync + 'static {
  fn spawn(&self, fut: BoxFuture<'static, ()>);
  fn spawn_local(&self, fut: LocalBoxFuture<'static, ()>) -> Result<(), NotLocal>;
  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>);
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()>;   // real timer; replaces rt.rs:134-139 thread::sleep
  fn now(&self) -> Instant;
}
pub struct TokioExecutor(tokio::runtime::Handle);          // default feature `tokio`
pub struct TestExecutor { /* virtual clock, deterministic queue */ }  // testing/test_runtime.rs successor

pub struct FiberId { id: u64, parent: Option<u64> }        // fiber_id.rs:11 gains parent
pub(crate) struct FiberContext {
  id: FiberId,
  interrupt: AtomicBool, waker: AtomicWaker,
  children: Mutex<Slab<Weak<FiberContext>>>, parent: Weak<FiberContext>,
  refs: FiberRefs,                                         // replaces thread-local fiber_ref.rs:28-40
}
pub struct FiberHandle<A, E> { ctx: Arc<FiberContext>, cell: Arc<ExitCell<A, E>> }
impl<A, E> FiberHandle<A, E> {
  pub fn join(self) -> Effect<A, E>;                       // no Clone+Sync bound (fixes rt.rs:27-28)
  pub fn await_exit(&self) -> Effect<Exit<A, E>> where A: Clone, E: Clone;
  pub fn interrupt(&self) -> Effect<Exit<A, E>>;           // request + await, Effect.ts semantics
}
```

- **Runloop.** `Runloop<A,E,R>: Future` drives one `Node` with an explicit frame stack. A fiber is
  `executor.spawn(Box::pin(runloop.then(|exit| cell.complete(exit))))`. `~sub` inside `effect!` runs a nested
  `Runloop` future that shares the parent `Ctx.fiber`, so it shares the interrupt state too.
- **Interrupt points.** The runloop checks `interrupt` (a) before every instruction (each bind), (b) before
  polling an `Async` future, and (c) whenever woken. Interrupting drops the in-flight future (cancellation),
  unwinds frames, and runs `Ensuring`/`acquire_release` finalizers with interruption masked.
- **Uninterruptible regions.** `Region { mask }` frames push and pop a per-runloop mask counter.
  `uninterruptible(eff)` and `uninterruptible_mask(|restore| …)` follow Effect.ts. While the mask is set, the flag
  stays pending and is honoured when the region exits.
- **Propagation.** `fork` registers the child under `parent.children`. When a fiber exits (by success, failure,
  or interrupt), it interrupts and awaits its live children before completing `ExitCell`. `fork_daemon` registers
  under the global root scope. `fork_in(scope)` ties the child to an explicit `Scope`.
  `FiberHandle::interrupt` stops failing a `Deferred` as it does today (`fiber_handle.rs:92-94`); it now actually
  stops the job (today's gap: `rt.rs:110-113`).
- **Edges.** `Runtime { exec: Arc<dyn Executor>, root: Arc<FiberContext> }` exposes `run(eff).await` (async edge)
  and `block_on(eff)`. `block_on` uses a real `Waker` built on `std::task::Wake` + `Thread::unpark` (safe Rust) and
  parks, replacing the `Waker::noop` + `yield_now` spin (`runtime/execute.rs:62-79`). Called inside a tokio
  worker, it returns `Err(BlockOnInAsync)` instead of deadlocking, which fixes the class of bug at
  `coordination/request_resolver.rs:102`. Internal `run_blocking(..).expect` noise for pure constructors
  (`fiber_handle.rs:49,60,65`) goes away: `Deferred::new()` and the like become plain fns.

## 2. Typed R

### 2.1 Representation

```rust
pub struct HNil;  pub struct HCons<H, T>(pub Arc<H>, pub T);
pub struct Here;  pub struct There<I>(PhantomData<I>);

#[diagnostic::on_unimplemented(
  message = "capability `{T}` is not in this effect's environment",
  label = "`{T}` is required here",
  note = "add `{T}` to the effect's `caps!(…)`, or remove it with `.provide(…)` / `.provide_layer(…)` before running")]
pub trait Selector<T, I> { fn get(&self) -> &Arc<T>; }
impl<T, Tl> Selector<T, Here> for HCons<T, Tl> { … }
impl<T, H, Tl: Selector<T, I>, I> Selector<T, There<I>> for HCons<H, Tl> { … }

pub trait Plucker<T, I> { type Rest; fn pluck(self) -> (Arc<T>, Self::Rest); fn plant(t: Arc<T>, rest: Self::Rest) -> Self; }
pub trait Embed<Sub, Is> { fn project(&self) -> Sub; }    // read-only subset view: Arc clones, no HashMap clone
pub trait Sculptor<Sub, Is> { type Rest; fn sculpt(self) -> (Sub, Self::Rest); fn plant(sub: Sub, rest: Self::Rest) -> Self; }
pub trait Concat<Rhs> { type Out; }
```

`caps!(Db, Logger)` becomes `HCons<Db, HCons<Logger, HNil>>`. The macro stays `macro_rules!`
(`crates/id_effect_macro/src/capability/caps.rs:16`), loses its 16-arity unroll, and can recurse. `Env` (the
`HashMap<TypeId, Arc<dyn Any>>` at `capability/env.rs:14-16`) survives as `DynEnv`, an explicit escape hatch whose
lookups are fallible only (`try_need -> Result<_, CapabilityError>`). That keeps D5's "need can never panic".

```rust
pub fn need<T: Send + Sync + 'static, R: Selector<T, I>, I>() -> Effect<Arc<T>, Never, R>;
impl<A, E, R> Effect<A, E, R> {
  pub fn provide<T, I>(self, t: T) -> Effect<A, E, <R as Plucker<T, I>>::Rest> where R: Plucker<T, I>;
  pub fn provide_some<Sub, Is>(self, sub: Sub) -> Effect<A, E, <R as Sculptor<Sub, Is>>::Rest>;
  pub fn provide_layer<O, E2, In, Is>(self, l: Layer<O, E2, In>)
    -> Effect<A, Or<E, E2>, <<R as Sculptor<O, Is>>::Rest as Concat<In>>::Out>;
  pub fn narrow<Wide, Is>(self) -> Effect<A, E, Wide> where Wide: Embed<R, Is>;  // what `~` does
}
```

`provide` compiles to a `Provide` frame. At runtime the frame takes the outer `Ctx<Rest>` and runs the inner
effect under `Ctx<R>` built by `plant`. Elimination happens at type level: `R::Rest` drops exactly one element.

### 2.2 Migration of the 0.6 capability surface

| 0.6 | 0.7 (A) |
|---|---|
| `Needs<T>::need(&r)` panics (`needs.rs:40-49,70`) | `Selector<T,_>::get` (compile-time); `need::<T>()` effect |
| `HasCap<K>` blanket (`set.rs:179-183`) | deleted; bound `R: Selector<T, I>` |
| `CapList<Ks>` + `CapKeys` 16-unroll (`set.rs:85-115,119-122`) | `HCons`/`HNil`, recursive |
| `project_at_0..7` (`set.rs:211-440`), `CapWiden` (`set.rs:197-460`) | `Embed<Sub, Is>` (any subset, any order) |
| `CapBind` runtime verify+panic (`cap_bind.rs:99-131`) | `narrow::<Wide, _>()` (compile-time, Arc clones) |
| `FromEnv`/`CapabilitySet::verify` in `run_with` (`run.rs:49-63`) | not needed: an `Effect<_,_,HNil>` is the only runnable type |
| `fiber_caps::with_override` thread-local overlay (`fiber_caps.rs:53-68`) | `provide` on a sub-effect (lexical) |

### 2.3 Providers → Layer (D7)

`Layer<Out, E, In>` is a memoised, scoped `Effect<Out, E, In>` where `Out` is an HList. Combinators:
`a.merge(b): Layer<Concat<OA,OB>, Or<EA,EB>, Concat<IA,IB>>`, `b.provide_to(a)` (feeds b's output into a's input),
`fresh()`, `scoped()`. `#[derive(ProviderSpecDerive)] #[provides(Db, requires(Config))]` keeps its syntax and now
generates `fn layer() -> Layer<caps!(Db), ProviderError, caps!(Config)>`. That replaces
`ProviderSpec::provide(&Env)` (`provider.rs:46`), `provide_effect` (`provider.rs:48`), and the async `on_shutdown`
(`provider.rs:70`, now a scope finalizer). The dynamic planner (`graph.rs:40`, `planner.rs:40`) stays only for
`DynEnv`.

### 2.4 Compile-error sketches (rustc 1.8x, abridged)

**(a) Missing capability**: `need::<Metrics>()` in an `Effect<_, _, caps!(Db, Logger)>`:

```text
error[E0277]: capability `Metrics` is not in this effect's environment
  --> src/users.rs:14:17
   |
14 |     let m = ~Metrics;
   |              ^^^^^^^ `Metrics` is required here
   |
   = help: the trait `Selector<Metrics, _>` is not implemented for `HNil`
   = note: required for `HCons<Logger, HNil>` to implement `Selector<Metrics, There<_>>`
   = note: 1 redundant requirement hidden
   = note: required for `HCons<Db, HCons<Logger, HNil>>` to implement `Selector<Metrics, There<There<_>>>`
   = note: add `Metrics` to the effect's `caps!(…)`, or remove it with `.provide(…)` …
```
Readability: good headline and label. The "required for" chain grows linearly with |R|, and rustc never prints
the alias. Acceptable up to about 6 caps, noisy at 12.

**(b) Ambiguous capability**: `Config` appears twice after `provide_layer` concatenates `Rest` with a layer input
that also needs `Config`:

```text
error[E0283]: type annotations needed
  --> src/app.rs:30:13
   |
30 |     let c = ~Config;
   |             ^^^^^^^ cannot infer type of the type parameter `I` declared on the function `need`
   |
   = note: multiple `impl`s satisfying `HCons<Config, HCons<Db, HCons<Config, HNil>>>: Selector<Config, _>` found
```
Readability: **poor**. `on_unimplemented` does not fire on ambiguity (E0283), so there is no custom text.

**(c) Wrong provide**: `.provide(Cache::new())` on an effect that never needed `Cache`:

```text
error[E0277]: the trait bound `HNil: Plucker<Cache, _>` is not satisfied
  --> src/main.rs:9:6
   |
 9 |     .provide(Cache::new())
   |      ^^^^^^^ `Cache` is not required by this effect, so it cannot be provided
   = note: required for `HCons<Db, HNil>` to implement `Plucker<Cache, There<_>>`
```

Mitigations:
1. `#[diagnostic::on_unimplemented]` on `Selector`, `Plucker`, `Embed`. These already exist in the style of
   `needs.rs:11-16`, `set.rs:174-178`, `cap_bind.rs:34-39`.
2. A `caps!` duplicate check (macro-time, by token).
3. `Layer::merge`/`provide_layer` route through a `NoDup` *debug* check at layer build (`TypeId` set), which panics
   in tests rather than prod. It cannot be static on stable Rust because there are no negative impls.
4. An `#[effect::check]` UI-test harness (trybuild) pins all three messages.
5. `#[diagnostic::do_not_recommend]` (stable since 1.85) on the `There` impl, to cut the chain to one line. **Must
   be verified in the spike.**

## 3. `effect!` / `pipe!` impact

What changes in `crates/id_effect_proc_macro`:
- `expand.rs:24-27` emits `r: &mut Ty`. This becomes `r: &Ty`, with `&mut` accepted plus a deprecation warning
  for one release.
- `expand.rs:33-35` uses `new_async` + `box_future`. It now emits `Node::Async(move |ctx| { let r = ctx.clone();
  Box::pin(async move { … }) })`, so the body owns `r`.
- `transform.rs:316`: `~Key` desugars to `Needs::<Key>::need(r)`. It now desugars to
  `Selector::<Key, _>::get(&*r)`, with the index inferred.
- `transform.rs:323`: `cap_into_bind(e, r).await?` becomes `e.narrow().run_in(&r).await?`. `narrow` is a no-op
  when `R == Wide` (a reflexive `Embed` impl) and `HNil` embeds in anything, which keeps the `R=()` behaviour at
  `cap_bind.rs:65-81`.
- `infer_caps.rs:8,150`: key collection is kept. With no annotation and no return type, the macro synthesises
  `caps!(collected…)`.
- `pipe!` (`crates/id_effect_macro/src/pipe/mod.rs:5-12`) is plain function application and stays unchanged.

User-visible effect: bodies must be `Send` (no `Rc`/`RefCell` across `~`), and generic helpers grow index
parameters.

```rust
// 0.6
type UserCaps = caps!(Db, Logger);
fn create_user(name: String) -> Effect<UserId, AppError, UserCaps> {
  effect!(|r: &mut UserCaps| {
    let db = ~Db;
    ~(~Logger).info(format!("creating {name}"));
    ~db.insert_user(&name)
  })
}
fn main() { run_with([provide!(DbLive), provide!(LoggerLive)], create_user("a".into())).unwrap(); }

// 0.7 / Design A
type UserCaps = caps!(Db, Logger);
fn create_user(name: String) -> Effect<UserId, AppError, UserCaps> {
  effect!(|r: &UserCaps| {                  // `&mut` -> `&`; body unchanged
    let db = ~Db;                           // &Arc<Db>; compile error if Db ∉ UserCaps
    ~(~Logger).info(format!("creating {name}"));
    ~db.insert_user(&name)
  })
}
// generic helper: the index leaks into the signature
fn audited<A, E, R, I>(e: Effect<A, E, R>) -> Effect<A, E, R> where R: Selector<Logger, I> { … }
#[tokio::main] async fn main() {
  let app = create_user("a".into()).provide_layer(DbLive::layer().merge(LoggerLive::layer()));
  Runtime::default().run(app).await.unwrap();  // R = HNil, statically
}
```

## 4. Fabric hook (D6)

Every parallel combinator goes through a single function:

```rust
pub(crate) fn fork_all<A, E, R>(ctx: &Ctx<R>, children: Vec<Effect<A,E,R>>, c: Concurrency) -> Effect<Vec<A>, E, R>;
pub enum Concurrency { Sequential, Bounded(usize), Unbounded, Inherit /* fabric decides */ }
pub trait Admission: Send + Sync { fn admit(&self, w: Weight) -> BoxFuture<'static, Permit>; }  // async, waker-based
```

- `zip_par`, `all(…, {concurrency})`, `for_each_par`, the `effect!` auto-parallel `join_binds*`
  (`kernel/par_bind.rs:73-100`, now try-join with sibling interrupt on first failure), and `Stream::map_effect_par`
  all call `fork_all`.
- `fork_all` forks children under the current fiber. With `Inherit` or `Bounded`, each fork first awaits
  `fabric.admission().admit(w)`.
- `AdmissionController` gains an async `admit` next to `acquire_blocking` (`compute/admission.rs:47`), which
  today blocks a thread inside tokio's blocking pool (`id_effect_tokio/src/lib.rs:193`). The blocking variant
  becomes `spawn_blocking`-only.
- `FiberPool` (`compute/fiber_pool.rs:17-66`) keeps CPU-bound `Effect::compute` work only.
- `ComputeFabric::spawn_admitted` (`compute/fabric.rs:101-111`) is re-expressed as `FabricExecutor<X: Executor>`,
  a decorator.

## 5. Risks, unknowns, spike

Ranked risks:
1. **R1: Duplicate elements → E0283 with no custom message.** This is intrinsic to HLists on stable Rust.
   Mitigation: the §2.4 checks, plus a rule that `Layer` composition produces the flat app env once at the edge.
   *Falsifier:* the showcase service (D10) cannot be wired without a manual `I` annotation or dedup.
2. **R2: Async-bind cost.** The `Box<dyn Any + Send>` per runloop frame can exceed +25% on async `flat_map`
   chains. Mitigation: a `SmallBox` inline slot for values ≤ 16 B; keep `Sync` fusion maximal.
3. **R3: Inference failures.**
   - (a) The index `I` leaks into every generic helper.
   - (b) An unannotated `effect!(|r| …)` passed to a generic function (no expected type) gives E0282, because the
     closure is type-checked before `R` is known.
   - (c) `need::<T>()` in `let` position, without later use, gives E0282.
   - (d) `R` order sensitivity: `caps!(A,B)` ≠ `caps!(B,A)` at fn boundaries. `~` narrows automatically, but
     plain assignment does not.
   Mitigation: an `.narrow()` suggestion in docs, and the `infer_caps.rs` synthesis.
4. **R4: Compile time.** Trait resolution is O(|R|) per lookup and O(|R|·|Sub|) per `Embed`. Unknown at 16 caps
   × 100 handlers.
5. **R5: `&mut R` → `&R`.** Anything that mutates env (`CapList::env_mut` / `DerefMut`, `set.rs:142-158`) must move to `Ref`
   services.
6. **R6: `do_not_recommend` behaviour** on recursive impls is unverified.

**Spike (≤ 1 wave)**: an unpublished workspace member `spikes/kernel_a/`. It is not under `labs/` (D: labs untouched).

| File | Content | LOC |
|---|---|---|
| `src/hlist.rs` | HNil/HCons/Here/There, Selector/Plucker/Embed/Concat + diagnostics | 220 |
| `src/effect.rs` | Node, Pure/Sync fusion, map/flat_map/catch/provide/narrow/need | 450 |
| `src/runloop.rs` | frame stack, interrupt checks, mask regions, ensuring | 350 |
| `src/fiber.rs` | FiberContext tree, FiberHandle/ExitCell, fork/fork_daemon/join/interrupt | 260 |
| `src/executor.rs` | Executor, TokioExecutor, block_on with park waker | 150 |
| `src/par.rs` | fork_all + Admission stub + zip_par/all | 130 |
| `tests/ui/*.rs` | trybuild: missing / ambiguous / wrong provide / generic-helper / unannotated closure | 120 |
| `tests/semantics.rs` | interrupt-propagation, mask, finalizer-on-interrupt, stack safety at 100k binds | 200 |
| `benches/kernel_a.rs` | see below | 220 |
| Total | | ~2.1k |

Benchmarks: run against 0.6 with the same criterion IDs where they exist.
- `phase0/bench_effect_map_chain` and `flat_map_chain` (`phase0_baseline.rs:7-34`): **gate ≤ 1.25×**.
- An async-bind chain of 64 `from_async` steps: gate ≤ 1.25×.
- `need` at depth 1/8/16 vs `env_lookup.rs`: expect ≥ 5× faster.
- Spawn+join of 10k fibers, and 1k concurrent `sleep(1ms)` against 0.6 `TokioRuntime`: **must improve** (D9).
- Interrupt latency (request → child finalizer run): report only.
- Compile-time: `cargo check` on a generated crate with 16 caps × 100 handlers. Falsify at > 2× vs the 0.6
  equivalent.

The design is falsified if any of these holds: the bind gate fails after the SmallBox mitigation; the UI tests show
E0283 is reachable in the showcase wiring; R3(b) needs annotations in more than 20% of the ported showcase handlers.

## 6. Migration impact (crates/*)

| Crate | Impact | Size |
|---|---|---|
| id_effect | kernel, runtime, capability, concurrency rewrite; 28 files with `run_blocking(` | L |
| id_effect_proc_macro | `effect!` expansion (`expand.rs`, `transform.rs`), provider derive → Layer | L |
| id_effect_macro | `caps!` recursive, `provide!`/`providers!` → layers | M |
| id_effect_fp | algebra over Effect (traversable/selective), STM global-lock spin | M |
| id_effect_tokio | becomes `TokioExecutor`; `spawn_with` blocking pool removed (`lib.rs:178-210`) | M |
| id_effect_axum | `EffectService` `F: Fn(&mut S, Req) -> Effect<Res,E,S>` (`service.rs`) → `Effect<_,_,HNil>` after layer; `run_local` path gone | M |
| id_effect_node | registry/mesh Send fibers, interrupt | M |
| id_effect_process | delete `SpinClock` (`clock_util.rs:4-9`) and node workaround | M |
| id_effect_config | `Config<A>` descriptor + layer | M |
| id_effect_platform | fs/http/auth providers → layers | M |
| id_effect_logger | thread-local globals (`lib.rs:76-125`) → FiberRefs/layer | M |
| id_effect_opentelemetry | span/metrics bridges on FiberContext | M |
| id_effect_fsm | saga/interpreter `run_blocking` removal | M |
| id_effect_sql | client/transaction as layers, Send | M |
| id_effect_resilience | retry → Schedule methods, rate limiter async | S |
| id_effect_cli | `run.rs` edge only | S |
| id_effect_optics | schema-serde dependency only (`Cargo.toml:21-23`) | S |
| id_effect_tower, id_effect_sql_pg | deleted (D8) | S |
