# Kernel Design B — trait-bound environment (`R: Has<Db> + Has<Log>`)

Status: DRAFT for comparison against Design A (HList + index inference). Covers D3, D4, D5, D6
together so `Effect<A, E, R>` breaks exactly once. Paths are relative to the
`release-0.7.0` worktree. Base: v0.6.0 (68d4b02).

## 0. Thesis in one paragraph

`R` stays an ordinary type parameter. Capability presence is a **nominal trait bound**
`R: Has<T>`. Library and service code is generic (`fn f<R: Has<Db> + Has<Log> + Env>()`), and
the application builds **one concrete env struct** (`#[derive(Env)] struct AppEnv { .. }`) at the
edge. `need::<Db>()` compiles only when `R: Has<Db>`, so it cannot panic. Sub-calls need no
runtime narrowing because trait bounds already give subsumption. That removes `CapBind`'s
clone + verify + panic path (`crates/id_effect/src/capability/cap_bind.rs:99-132`). The price is
that **`R ∪ {T}` cannot be expressed for a generic `R`** (§2.3). Narrowing works through concrete
structs, not type-level unions.

The workspace already does this in places. `id_effect_config` writes `R: Needs<ConfigProviderService>`
(`crates/id_effect_config/src/load.rs:53,73,91`) and platform writes `R: Needs<ReqwestClient> + CapBindR`
(`crates/id_effect_platform/src/http/reqwest.rs:79,137`). Design B makes that pattern the only
one and makes the bound mean something. Today `Needs` has a blanket impl for any `Deref<Target=Env>`
(`capability/needs.rs:40-50`), and `HasCap` has one too (`capability/set.rs:183`).

## 1. Core signatures (D3 + D4)

### 1.1 Representation

Today every combinator allocates `Box<dyn EffectOp>` (`kernel/effect.rs:215`) and also does a
`Box::pin` of an `async` block (`effect.rs:277-282`, `309-314`). Even a ready `Result` bind boxes
(`effect.rs:118-120`). We compared three options:

| Option | Allocs per `flat_map` | Interrupt/fiber hooks | Verdict |
|---|---|---|---|
| (a) Enum-of-ops tree + interpreter (ZIO/Effect.ts `FiberRuntime`) | node + continuation + `Box<dyn Any>` for each intermediate value (Rust needs type erasure for a heterogeneous continuation stack) | trivial, since the loop sees every op | likely **worse** than today on bind and loses `async` interop. Rejected. |
| (b) Fused generic future (`impl EffectT<A,E,R>`) | 0 | needs a wrapper at each await | types explode, recursion needs boxing, and with a generic `R` it multiplies monomorphization. Rejected for the public type. |
| (c) **Step enum with inline ready variants + one boxed `Send` future** | 0 for pure/ready, 2 for async (same as today) | interrupt check at bind + interruptible leaves | **Chosen** |

```rust
pub struct Effect<A, E = Never, R = ()> { step: Step<A, E, R> }
enum Step<A, E, R> {
  Ready(Result<A, Cause<E>>),                                    // succeed/fail/Result bind: no alloc
  Sync(Box<dyn FnOnce(&mut R) -> Result<A, Cause<E>> + Send>),
  Async(Box<dyn for<'a> FnOnce(&'a mut R) -> BoxFuture<'a, Result<A, Cause<E>>> + Send>),
}
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;   // was !Send, effect.rs:73
```

- `map`/`flat_map` on `Ready` apply eagerly with no allocation. `Sync∘Sync` fuses into one `Sync`
  closure (one box, no `Box::pin`). Only an `Async` left side pays the 2 allocations it pays today.
- The internal error channel becomes `Cause<E>`, which is required for interrupt and for D7's
  panic-preserving `Cause::Die`. Today `Cause::Die(String)` is at `failure/cause.rs:15-26`.
  Public `E` stays typed. `run` returns `Exit<A, E>`.
- Budget argument (D9 ≤ 25%): the steady-state async bind does today's 2 allocations plus one
  relaxed atomic load (interrupt flag) plus a thread-local read (current fiber). The pure and
  ready paths should get faster. The spike measures this (§5).

### 1.2 `LocalEffect` (D4)

`LocalEffect<A, E, R>` has the same three-variant layout without the `Send` bounds. Both types are
generated from one `kernel_impl!` macro_rules template so the combinators exist once in source.
`impl From<Effect<..>> for LocalEffect<..>` is free. The reverse does not exist. `LocalEffect` runs
only on a `LocalExecutor` (`spawn_local`). Streams that rely on `Rc` today
(`effect.rs:70` doc, `streaming/stream.rs` tests) either become `Send` (`Arc<Mutex<StreamState>>`
already, `stream.rs:223`) or live on `LocalStream`.

### 1.3 `Executor`, fibers, ids

```rust
pub trait Executor: Send + Sync + 'static {
  fn spawn(&self, fiber: Pin<Box<dyn Future<Output = ()> + Send>>);
  fn spawn_blocking(&self, f: Box<dyn FnOnce() + Send>);
  fn sleep(&self, d: Duration) -> BoxFuture<'static, ()>;
  fn now(&self) -> Instant;
}
pub trait LocalExecutor { fn spawn_local(&self, fiber: Pin<Box<dyn Future<Output = ()>>>); }

pub struct FiberId(u64);                     // keep concurrency/fiber_id.rs:11
pub(crate) struct FiberState {               // Arc-shared
  id: FiberId, parent: Option<Weak<FiberState>>,
  children: Mutex<Vec<Weak<FiberState>>>,
  interrupted: AtomicBool, waker: AtomicWaker, mask: AtomicU32 /* uninterruptible depth */,
  refs: FiberRefs,
}
pub struct FiberHandle<A, E> { state: Arc<FiberState>, exit: Arc<ExitCell<A, E>> }
```

- `FiberRuntime<A,E,R>` is the interpreter loop. It is a `Future<Output=()>` that owns `R`, the
  root `Step`, and `Arc<FiberState>`. Around each `poll` it sets a thread-local `CURRENT_FIBER`.
  Binds and leaves read it to check interrupt and fork children. This is safe (no `unsafe`) because
  `poll` is synchronous on one thread, and there is precedent in `capability/fiber_caps.rs:19-50`.
- `ExitCell` is a one-shot slot plus a waker list. `join` *takes* the exit and observers get
  `Arc<Exit>`. This drops the `A, E: Clone + Send + Sync` requirement that the `watch`-based
  `Deferred` forces today (`coordination/deferred.rs:18-27`, `runtime/rt.rs:27-28`).
- The tokio backend is `tokio::spawn` with real wakers. This replaces `spawn_blocking` +
  `run_blocking` (`crates/id_effect_tokio/src/lib.rs:191-195`).
- A built-in `ParkExecutor` (thread-park waker + timer thread) serves `run_blocking` and builds
  without tokio (ADR 0010 deferred item, `docs/adrs/0010-crate-tiers.md:89-93`).

### 1.4 Interrupt model

- **Request:** `handle.interrupt()` returns `Effect<Exit<A,E>, Never, R>`. It sets the flag,
  wakes the fiber, and **awaits** the exit (Effect.ts semantics). `interrupt_fork` does not wait.
  Today interrupt only fails a `Deferred` (`concurrency/fiber_handle.rs:92-94`) and the job keeps
  running (`runtime/rt.rs:110-113`).
- **Interrupt points:**
  1. Before running each `flat_map` continuation.
  2. Every suspending leaf. `from_async`, `sleep`, `Deferred::await`, `Queue::take` and
     `Semaphore::acquire` are wrapped in `Interruptible<F>`, which polls the fiber's
     `AtomicWaker` first, so a parked fiber wakes on interrupt.
  3. `yield_now`.
  Pure `Sync` closures are not interruptible, by design.
- **Uninterruptible regions:** `uninterruptible(eff)` and `uninterruptible_mask(|restore| ..)`
  bump `mask`. Interrupt points skip while `mask > 0`. Finalizers from `ensuring`, `on_exit` and
  `acquire_release` always run masked. Today's `ensuring` is at `effect.rs:541-549`.
- **Children:** `fork` registers a child in the parent's `children`. When the parent exits for any
  reason, it interrupts and awaits all live non-daemon children (structured concurrency).
  `fork_daemon` attaches to the root scope. `fork_in(scope)` attaches to a `Scope`. The `parent`
  argument that is ignored today (`rt.rs:34,43,128`) becomes real.
- **Drop cancellation:** if the executor drops a fiber future (runtime shutdown), async finalizers
  cannot run. `Runtime::shutdown` therefore interrupts root fibers and awaits them before
  dropping the executor. This is documented as a hard limit.

### 1.5 `run` / `run_blocking` survive only at edges

- `run_blocking(eff, env)` builds or uses a `ParkExecutor`, spawns a root fiber, and parks the
  thread until it is woken. This replaces the `Waker::noop` + `yield_now` spin at
  `runtime/execute.rs:62-79`. If `CURRENT_FIBER` is set it **panics with a diagnostic**
  ("run_blocking inside a fiber; use `~eff`"). That makes the
  `coordination/request_resolver.rs:102` class of bug loud instead of a deadlock.
- `run(eff, env).await` is `Send` and spawns on the ambient executor.
- Pure constructors (`Deferred::new`, `Semaphore::new`, `Cache::new`) become plain functions.
  That removes most of the `run_blocking(..).expect` noise, for example `fiber_handle.rs:49,93`.

## 2. Typed R

### 2.1 Traits

```rust
#[diagnostic::on_unimplemented(
  message = "effect environment `{Self}` does not provide capability `{T}`",
  label = "`{T}` is required here",
  note = "add a `{T}` field to your `#[derive(Env)]` struct, or add `Has<{T}>` to the `R: …` bounds")]
pub trait Has<T: Service> { fn get(&self) -> &T; }
pub trait Replace<T: Service>: Has<T> { fn replace(&mut self, t: T) -> T; }
pub trait Env: Clone + Send + Sync + 'static {}      // `()` deliberately does NOT impl Env
pub trait Service: Clone + Send + Sync + 'static {}  // same bound as Needs, needs.rs:19
```

`#[derive(Env)]` on a struct emits `Env`, plus `Has<F>` and `Replace<F>` for every field type
(rejecting duplicate field types up front). It also emits an exported forwarding macro
`__id_effect_forward_<Name>!` that downstream structs use for `#[env(flatten)] base: BaseEnv`.
`caps!` changes from a type (`id_effect_macro/src/capability/caps.rs:16-212`) to a **trait
alias** generator: `caps!(pub trait OrderCaps = Database + Mailer);` expands to
`trait OrderCaps: Env + Has<Database> + Has<Mailer> {}` plus a blanket impl.

### 2.2 The two variants

- **B1 (recommended): generic `R` in library and service code**, concrete `R` only at the edge.
  `fn f<R: Has<Db>>() -> Effect<A, E, R>`. Calling `g::<R>()` (which needs only `Has<Db>`) from
  `f` type-checks by bound entailment, with zero runtime cost.
- **B2: a concrete struct `R` per function** (`Effect<A, E, OrderEnv>`). Every call from a wider
  env needs an explicit `provide_env` projection. B2 is what you get when B1 code is instantiated
  at a concrete type, and it is not a separate design. We reject it as the *authoring* style
  because it brings back per-call narrowing.

### 2.3 `provide` / `provide_some`: the hard part

An Effect.ts `provide` *removes* a service from `R`. With trait bounds, removing a service from a
generic `R` means building an `R' = R + T` type. Every encoding of that needs one of these:

```rust
impl<T, R> Has<T> for With<T, R> { .. }                // head
impl<T, U, R: Has<U>> Has<U> for With<T, R> { .. }     // tail — E0119 overlap when U == T
```

Making that coherent needs specialization (unstable) or an index witness `Has<T, I>`, and the
index witness *is* Design A. Faking it with a `TypeId` fallback re-introduces runtime panics,
which violates D5. So Design B offers exactly these four operations:

| Operation | Signature | Static? |
|---|---|---|
| Eliminate env (edge) | `eff.provide(env: R) -> Effect<A, E, ()>` | yes |
| Project / re-shape (concrete) | `eff.provide_env(f: FnOnce(&mut S) -> R) -> Effect<A,E,S>` (rename of `zoom_env`, `effect.rs:524-535`) | yes |
| Override existing service (generic) | `eff.provide_service::<T>(t) where R: Replace<T>` (clone `R`, replace slot; generalizes `local`, `effect.rs:484-496`) | yes |
| Add a service (generic `R`) | **not supported generically**. Declare an extension struct: `#[derive(Env)] struct WithUser<R> { #[env(base(forward(Db, Log)))] base: R, user: CurrentUser }` with an explicit forward list (coherent because the forwarded and own types are concrete and disjoint), or concrete `#[env(flatten)] app: AppEnv` | yes |

`provide_some` is therefore spelled "build a bigger struct". The axum per-request overlay is the
main consumer: `RequestCaps` mutates a dynamic `Env` (`crates/id_effect_axum/src/request_caps.rs:62-108`).
It becomes `#[derive(Env)] struct RequestEnv { #[env(flatten)] app: AppEnv, user: CurrentUser }`
built in the extractor. That is one struct per app, not one per handler.

**Layer (D7) under B.** `Layer<Out, E, In>` is a memoized, scoped constructor
`In -> Effect<Out, E, In>` where `Out: Service` or `Out: Env`. `fresh`, `scoped` and `provide`
(feeding one layer's output into another's `In` when the types match) work. **`Layer::merge`
cannot yield a type-level union.** It yields a tuple `(A, B)`, and the user names the union with a
derived struct (`AppEnv::from_parts`). Apps assemble in strata (`BaseEnv{config, clock}` →
`InfraEnv{base, db, http}` → `AppEnv{infra, mailer}`) with `#[env(flatten)]`. This is the main
loss of parity versus Effect.ts and the main argument for Design A.

### 2.4 Migration of existing capability machinery

| Today | Design B |
|---|---|
| `Env` TypeId map (`capability/env.rs:14-16`) | kept only as opt-in `DynEnv` for plugin/dynamic cases. It implements `TryHas<T>` (returns `Result`), **never** `Has<T>` |
| `CapList<Ks>` + `HasCap` blanket (`set.rs:117-183`) | deleted; `caps!` becomes a trait-alias generator |
| `Needs<T>` panicking `need` (`needs.rs:17-51`) | renamed to `Has<T>`; `Needs` kept one release as a `#[deprecated]` alias trait |
| `project_at_0..7`, `CapWiden` (`set.rs:185-260+`) | deleted; subsumption is bound entailment |
| `CapBind` narrowing (`cap_bind.rs:99-132`) | deleted. The env-free impl (`cap_bind.rs:69-81`) survives as `IntoBind<R> for Effect<A,E,()> where R: Env`. It stays coherent next to `IntoBind<R> for Effect<A,E,R>` only because `(): !Env`, which is the same trick used today with `CapBindR` |
| `ProviderSpec`/graph planner (`provider.rs:19-71`, `graph.rs:92-97`), `run_with` (`run.rs:49-63`) | `#[derive(ProviderSpec)]` becomes `#[derive(Layer)]`; `run_with([provide!(..)], app)` becomes `AppEnv::layer().build()` then `run(app, env)`. `on_shutdown` becomes async scope finalizers (fixes the sync hook, `provider.rs:48,70`) |

### 2.5 Compile-error sketches

**(1) Missing capability at the edge:**
```
error[E0277]: effect environment `AppEnv` does not provide capability `Mailer`
  --> src/main.rs:31:9
31 |     run(process_order(id), env)
   |         ^^^^^^^^^^^^^ `Mailer` is required here
   = note: add a `Mailer` field to your `#[derive(Env)]` struct, or add `Has<Mailer>` to the `R: …` bounds
note: required by a bound in `process_order`
  --> src/orders.rs:12:35
12 | pub fn process_order<R: Env + Has<Database> + Has<Mailer>>(…)
```
**Missing inside generic code** is rustc-native: `help: consider further restricting type
parameter R with Has<Mailer>`. Readability is **good**. Errors name the env, the capability and
the originating bound. There are no index types, unlike Design A's likely
`E0283 type annotations needed: cannot satisfy HasAt<Mailer, _>`.

**(2) Conflicting capability:**
```
error: `Database` is provided twice in `AppEnv` (fields `primary`, `replica`)
  --> src/env.rs:4:10   = help: wrap one in a newtype (`struct ReplicaDb(Database);`)
```
The derive detects duplicates when they are syntactically the same type. Duplicates that come in
through `flatten` or a type alias surface as raw `E0119 conflicting implementations of Has<Database>`
pointing at the derive. Readability is **fair**. `on_unimplemented` cannot customize E0119.

**(3) Wrong provide:**
```
error[E0277]: the trait bound `AppEnv: Replace<MockMailer>` is not satisfied
  --> tests/orders.rs:20:10
20 |     eff.provide_service(MockMailer::default())
   = note: the env slot is `Mailer` (= Arc<dyn MailerApi>); pass `Arc::new(MockMailer…) as Mailer`
```
Readability is **fair to good**. We add `on_unimplemented` on `Replace` with a "slot type" note.
A related trap is `E0282 type annotations needed for Effect<_, _, R>` when a generic effect is
built outside `effect!` with no anchor. Inside `effect!` the closure parameter `|r: &mut R|`
anchors `R`.

## 3. `effect!` / `pipe!` impact

Codegen today: `~Key` → `Needs::<Key>::need(r)` (`id_effect_proc_macro/src/transform.rs:314-319`),
`~expr` → `cap_into_bind(expr, r).await?` (`transform.rs:323`), `new_async` + `box_future`
(`expand.rs:31-36`). Under B:

- `~Key` and `require!(Key)` → `<_ as Has<Key>>::get(r)`, with the same borrow/clone semantics as
  today's `need`.
- `~expr` → `into_bind(expr, r).await?`, same `R` with no narrowing. Interrupt checks live in
  `flat_map`/leaves, not in the macro.
- `infer_caps::validate_explicit_caps` (`expand.rs:18-23`, `infer_caps.rs:150-168`) is replaced by
  rustc bound checking. An optional `#[effect_fn]` attribute can add `Has<K>` bounds for directly
  used `~K` keys (`infer_caps.rs:8`), but transitive needs still come from rustc's
  "consider restricting R" help.
- Auto-parallel `join_binds2..4(.., r.clone())` (`effect_graph.rs:115-143`) → `zip_par`/`all`
  (§4), which needs `R: Env` (Clone + Send).
- The body must now be a `Send` future. A `!Send` local held across `~` gives the noisy
  "future cannot be sent between threads safely" error. Mitigation: `effect_local!` emits
  `LocalEffect`.
- `pipe!` (`id_effect_macro/src/pipe/mod.rs:5`) is value-level and unchanged.

**Before** (`crates/id_effect/book/src/appendix-b-migration.md:65-74`):
```rust
pub fn process_order(order_id: OrderId) -> Effect<Receipt, AppError, caps!(Database, Mailer)> {
  effect!(|r: &mut caps!(Database, Mailer)| {
    let db = require!(Database); let mailer = require!(Mailer);
    let order = ~ db.get_order(order_id);
    let receipt = ~ db.complete_order(order);
    ~ mailer.send_receipt(&receipt);
    receipt
  })
}
// edge: run_with([provide!(DbLive), provide!(MailerLive)], process_order(id))  — panics if a cap is missing
```
**After:**
```rust
caps!(pub trait OrderCaps = Database + Mailer);

pub fn process_order<R: OrderCaps>(order_id: OrderId) -> Effect<Receipt, AppError, R> {
  effect!(|r: &mut R| {
    let db = ~Database; let mailer = ~Mailer;
    let order = ~ db.get_order(order_id);
    let receipt = ~ db.complete_order(order);
    ~ mailer.send_receipt(&receipt);
    receipt
  })
}

#[derive(Clone, Env)]
struct AppEnv { db: Database, mailer: Mailer, #[env(flatten)] base: BaseEnv }
// edge: let env = AppEnv::layer(DbLive, MailerLive).build_scoped().await?;
//       let exit = rt.run(process_order(id), env).await;   // missing cap = compile error
```
What users change: they add `<R: …>` to signatures, the `caps!` *type* becomes a trait alias, the
closure annotation becomes `&mut R`, providers become a derived env, and bodies must be `Send`.

## 4. Fabric hook (D6)

```rust
pub enum Concurrency { Sequential, Bounded(usize), Unbounded, Inherit /* = Fabric budget */ }
pub fn zip_par<A,B,E,R: Env>(a: Effect<A,E,R>, b: Effect<B,E,R>) -> Effect<(A,B),E,R>;
pub fn all<A,E,R: Env>(effs: impl IntoIterator<Item=Effect<A,E,R>>, c: Concurrency) -> Effect<Vec<A>,E,R>;
pub trait Scheduler: Send + Sync { fn admit(&self, hint: TaskHint) -> BoxFuture<'static, Permit>; }
```
- Each branch is `fork`ed as a **child fiber** of the current fiber with a cloned `R`. Before the
  fork, `Scheduler::admit(..).await` runs. `ComputeFabric` implements `Scheduler` through an
  **async** permit on `AdmissionController`, which replaces the `yield_now` spin in
  `acquire_blocking` (`compute/admission.rs:47-51`) and `spawn_admitted` (`compute/fabric.rs:101-111`).
- With `Inherit`, the parallelism is the Fabric's live permit budget. Without the `compute`
  feature, the default `Scheduler` is unbounded.
- Fail-fast: the first failure interrupts siblings through the fiber tree, giving try-join
  semantics. That fixes `join` vs `try_join` (`kernel/par_bind.rs:88-99`) and removes the
  arity ≤ 4 limit. This also delivers ADR 0007 Phase C `par_all` (`docs/adrs/0007-compute-fabric.md:40-47`).
- Under B, the "no overlapping `&mut R` borrows" safety gate (ADR 0007:45) holds by construction,
  because each child owns a clone of `R`.

## 5. Risks, unknowns, spike

| # | Risk | Sev | Mitigation / falsifier |
|---|---|---|---|
| R1 | No generic `R ∪ T`; `Layer::merge` yields a tuple, not a union | **High** | Derived extension structs + strata. **Falsifier:** porting `request_caps.rs` + showcase needs more than one extra env struct per app, or forward lists exceed about 5 entries → choose Design A |
| R2 | Send-by-default error noise in `effect!` bodies | High | `effect_local!`; trybuild UI tests for the 3 common `!Send` traps |
| R3 | Bind cost with `Cause<E>` + Send + interrupt check | Med | `Ready`/`Sync` fast paths. **Falsifier:** async bind chain more than +25% vs `benches/phase0_baseline.rs` |
| R4 | Cross-crate `flatten` relies on derive-emitted `macro_rules` forwarding | Med | Spike one cross-crate flatten (logger env inside app env) |
| R5 | Generic `R` verbosity / compile time (monomorphization per env) | Med | Boxed `Step` keeps per-instantiation code small; measure `cargo build -p id_effect_platform` time |
| R6 | Streams have `Rc` assumptions | Med | Audit `streaming/` (10 files); `LocalStream` fallback |
| R7 | Drop-cancel skips async finalizers at executor shutdown | Low-Med | `Runtime::shutdown` interrupts roots first; documented |
| R8 | Thread-local `CURRENT_FIBER` under work stealing | Low | Set/clear around each `poll` only |

**Unknowns:** `catch_unwind` placement (per fiber poll vs per `Sync` step) for `Cause::Die`
payload; whether `on_unimplemented` notes render for `Has<T>` reached through a `caps!` alias
(MSRV 1.94 per `Cargo.toml:32`, so the attribute is available).

**Spike (one wave, behind `feature = "kernel_v2"` in `crates/id_effect/src/kernel_v2/`, about 2.0k LOC):**

| File | LOC |
|---|---|
| `kernel_v2/effect.rs`: `Step`, `succeed/fail/sync/from_async/map/flat_map/ensuring/uninterruptible` | 450 |
| `kernel_v2/local.rs`: `kernel_impl!` twin → `LocalEffect` | 120 |
| `kernel_v2/executor.rs`: `Executor`, `TokioExecutor`, `ParkExecutor`, `run`/`run_blocking` | 280 |
| `kernel_v2/fiber.rs`: `FiberRuntime`, `FiberState`, `FiberHandle`, `ExitCell`, interrupt, children | 450 |
| `kernel_v2/env.rs`: `Has/Replace/Env/Service`, `provide/provide_env/provide_service` | 120 |
| `kernel_v2/par.rs`: `zip_par`, `all`, `Scheduler` + Fabric async admission | 180 |
| `id_effect_proc_macro/src/derive_env.rs`: duplicate check, flatten, forward macro | 280 |
| `crates/id_effect/tests/ui_kernel_v2/*.rs`: the 3 error sketches + `!Send` + cross-crate flatten | 120 |

**Benches** (new `benches/kernel_v2.rs`, compared with `phase0_baseline.rs` and
`id_effect_tokio/benches/startup_first_spawn.rs`):
1. 1k `flat_map` chain (sync, async, ready).
2. 10k spawn+join.
3. 10k fibers each `sleep(1ms)`: wall time and CPU%, which must be far below the 0.6 spin.
4. Interrupt latency of a sleeping fiber at p50/p99, target < 100 µs.
5. `all(1k, Inherit)` with Fabric.
6. `process_order` on `AppEnv` 100k× on a tokio multi-thread runtime, as the axum-path proxy.

**Design falsifiers:** R1, R3, or interrupt latency > 1 ms.

## 6. Migration impact per stable crate

| Crate | Impact | Size |
|---|---|---|
| id_effect | kernel, runtime, concurrency, capability rewrite; 416 `Effect<` sites, 143 `caps!/CapList/HasCap`; most `run_blocking` uses go | **L** |
| id_effect_proc_macro | `effect!` codegen (`transform.rs`, `expand.rs`, `effect_graph.rs`), `infer_caps` shrink, new `derive(Env)`, `ProviderSpec` → `Layer` derive | **L** |
| id_effect_macro | `caps!` → trait alias; `provide!/providers!` → layer helpers; `require!` stays | S |
| id_effect_tokio | `Runtime` impl → `Executor` (`lib.rs:178-215`); delete `spawn_blocking_run_async` | S |
| id_effect_axum | delete the `EffectExecutor` thread pool (`executor.rs:237-428`, a `!Send` workaround); `RequestCaps` → `RequestEnv` | M |
| id_effect_config | `R: Needs<..>` → `R: Has<..>` (`load.rs`); mechanical | S |
| id_effect_platform | `R: Needs<..> + CapBindR` → `R: Has<..> + Env` (`http/reqwest.rs`) | S-M |
| id_effect_logger | thread-local globals (`lib.rs:76-125`) → `Has<Logger>` service | M |
| id_effect_process / id_effect_node | drop `SpinClock`/`node.rs` workarounds; mailbox on real wakers | M |
| id_effect_fp | STM spin (`stm/mod.rs:9-28`) onto wakers; Reader untouched | M |
| id_effect_resilience | `hedged` → `race`; Schedule moves (D7) | S |
| id_effect_sql | `run_blocking` sites → async | S-M |
| id_effect_fsm, id_effect_opentelemetry, id_effect_cli | signature churn | S |
| id_effect_optics | none | — |
| id_effect_tower, id_effect_sql_pg | pruned (D8) | — |

## 7. Self-scrutiny

The leaf most likely to be wrong is **§2.3's claim that one extension struct per app is enough**.
If real handlers need many different per-request additions (tenant, user, span, tx), the forward
lists grow and Design A's type-level union wins. For B to be the wrong design overall, you would
have to believe that Effect.ts-style `Layer.merge` unions are central to idiomatic use and not an
edge-assembly detail. Port `request_caps.rs` and the showcase env in the spike before deciding.
