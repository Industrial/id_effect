---
name: effect.rs-fundamentals
description: >-
  Idiomatic Effect.rs: program structure, HARD rules, and BAD/GOOD patterns for every
  major construct in the `effect` crate (foundation through testing). Use when writing
  or refactoring Effect-based Rust, wiring services/layers, schema, streams, STM, or
  converting async/Result code to Effect.
category: framework
displayName: Idiomatic Effect.rs
color: purple
---

# Idiomatic Effect.rs

This document is the **single source of truth** for how to structure code and use the `effect` crate in this repository. It is written as a compact book: **every illustrative example is shown as a BAD pattern and a GOOD pattern**—no exceptions.

**Audience:** humans and agents implementing or reviewing Effect.rs code.

---

## How to structure your codebase

Organize crates so **domain logic** stays generic in `A`, `E`, and `R`; **infrastructure** provides `Layer`s and concrete `Context` stacks; **binaries and tests** are the only places that call `run_blocking` / `run_async` / `run_test*`.

**BAD** — `run_blocking` sprinkled through library crates “for convenience.”

```rust
pub fn load_user(id: u64) -> User {
    run_blocking(fetch_user_effect(id), build_env()).expect("user")
}
```

**GOOD** — libraries return `Effect`; the binary (or a dedicated harness) runs it.

```rust
pub fn fetch_user<A, E, R>(id: u64) -> Effect<A, E, R>
where
    A: From<User> + 'static,
    E: From<DbError> + 'static,
    R: NeedsDb + 'static,
{
    effect!(|_r: &mut R| {
        let db = ~DbService;
        let u = ~db.get_user(id);
        A::from(u)
    })
}

fn main() {
    let _ = run_blocking(fetch_user::<User, _, _>(1), build_env());
}
```

Within a crate, keep **one `effect!` block per graph-building function** (control flow inside the macro), group **pure helpers** next to **effect steps**, and put **wire parsing** (`schema`) at IO boundaries—not inside deep domain graphs unless that is the domain.

---

## HARD project rules (non‑negotiable)

These rules are **mandatory** for composable library code. Each rule below includes BAD/GOOD.

### 1) No new `async fn` application functions

**BAD**

```rust
pub async fn download(url: String) -> Result<Bytes, reqwest::Error> {
    reqwest::get(url).await?.bytes().await
}
```

**GOOD**

```rust
pub fn download<A, E, R>(url: String) -> Effect<A, E, R>
where
    A: From<Bytes> + 'static,
    E: From<reqwest::Error> + 'static,
    R: 'static,
{
    from_async(move |_r| async move {
        let b = reqwest::get(url).await?.bytes().await?;
        Ok(A::from(b))
    })
}
```

### 2) Library functions stay generic over `R` (no concrete `Context<Cons<…>>` in public APIs)

**BAD**

```rust
pub fn work() -> Effect<(), MyErr, Context<Cons<Service<MyKey, MySvc>, Nil>>> {
    succeed(())
}
```

**GOOD**

```rust
pub fn work<A, E, R>() -> Effect<A, E, R>
where
    A: Default + 'static,
    E: From<MyErr> + 'static,
    R: NeedsMySvc + 'static,
{
    effect!(|_r: &mut R| {
        let _svc = ~MySvc;
        A::default()
    })
}
```

### 3) Services via `~Tag` inside `effect!`, not as function parameters

**BAD**

```rust
pub fn step(log: &EffectLogger) -> Effect<(), LogErr, ()> {
    log.info("hi")
}
```

**GOOD**

```rust
pub fn step<A, E, R>() -> Effect<A, E, R>
where
    A: Default + 'static,
    E: From<LogErr> + 'static,
    R: NeedsEffectLogger + 'static,
{
    effect!(|_r: &mut R| {
        let log = ~EffectLogger;
        ~log.info("hi");
        A::default()
    })
}
```

### 4) Honest `where` clauses (`E: From<…>`, `R: Needs…`)

**BAD** — missing `From` bounds, then forcing `unwrap` inside.

```rust
pub fn f<A, E, R: 'static>() -> Effect<A, E, R>
where
    A: Default + 'static,
{
    effect!(|_r: &mut R| {
        let _ = ~EffectLogger; // may not compile: E not constrained
        A::default()
    })
}
```

**GOOD**

```rust
pub fn f<A, E, R>() -> Effect<A, E, R>
where
    A: Default + 'static,
    E: From<EffectLoggerError> + 'static,
    R: NeedsEffectLogger + 'static,
{
    effect!(|_r: &mut R| {
        let log = ~EffectLogger;
        ~log.info("hi");
        A::default()
    })
}
```

### 5) Prefer `NeedsX` supertraits over raw `Get<…>` at call sites

**BAD**

```rust
pub fn g<E, R>() -> Effect<(), E, R>
where
    R: Get<EffectLogKey, Here, Target = EffectLogger> + 'static,
{
    succeed(())
}
```

**GOOD**

```rust
pub fn g<E, R>() -> Effect<(), E, R>
where
    E: 'static,
    R: NeedsEffectLogger + 'static,
{
    succeed(())
}
```

### 6) Domain errors with `fail(…).into()` into `E`

**BAD**

```rust
~fail(my_domain_err); // if fail expects E and my_domain_err is another type without into()
```

**GOOD**

```rust
~fail::<A, E, R>(MyDomainError::Bad.into());
```

### 7) Composable graph builders use `<A, E, R>` — never `<E, R>` with a fixed success type

**BAD**

```rust
pub fn load_df<E, R>() -> Effect<DataFrame, E, R> {
    succeed(DataFrame::new())
}
```

**GOOD**

```rust
pub fn load_df<A, E, R>() -> Effect<A, E, R>
where
    A: From<DataFrame> + 'static,
    E: 'static,
    R: 'static,
{
    effect!(|_r: &mut R| {
        let df = DataFrame::new();
        A::from(df)
    })
}
```

*Narrow exception:* small **service methods** on a `Copy` service struct may return `Effect<(), SvcErr, R>` without abstracting `A`—they are not general graph nodes.

### 8) No `_effect` suffix on graph builders; use `*_blocking` only for true runners

**BAD**

```rust
pub fn calc_kelly_fraction_effect<A, E, R>(...) -> Effect<A, E, R> { ... }
```

**GOOD**

```rust
pub fn calc_kelly_fraction<A, E, R>(...) -> Effect<A, E, R> { ... }

pub fn calc_kelly_fraction_blocking(...) -> Result<A, E> {
    run_blocking(calc_kelly_fraction::<A, E, ()>(...), ())
}
```

---

## The `effect!` macro — shape and discipline

**BAD** — multiple `effect!` bodies selected at function scope.

```rust
pub fn branch<A, E, R>(flag: bool) -> Effect<A, E, R>
where
    A: Default + 'static,
    E: 'static,
    R: 'static,
{
    if flag {
        effect!(|_r: &mut R| { A::default() })
    } else {
        effect!(|_r: &mut R| { A::default() })
    }
}
```

**GOOD** — one macro; control flow inside.

```rust
pub fn branch<A, E, R>(flag: bool) -> Effect<A, E, R>
where
    A: Default + 'static,
    E: 'static,
    R: 'static,
{
    effect!(|_r: &mut R| {
        if flag {
            // ...
        } else {
            // ...
        }
        A::default()
    })
}
```

**BAD** — obsolete postfix-tilde forms.

```rust
// x ~ step;   // old
```

**GOOD**

```rust
let x = ~step;
```

**BAD** — `from_async` future doing error work that `?` already does.

```rust
let v = ~from_async(move |_r| async move {
    match external().await {
        Ok(v) => Ok(v),
        Err(e) => Err(E::from(e)),
    }
});
```

**GOOD**

```rust
let v = ~from_async(move |_r| async move { external().await.map_err(E::from) });
```

---

## `foundation` — pure bedrock (no `Effect`)

### `unit` (`Unit`, `discard`, `extend`)

**BAD** — using random `()` to mean “success” in public APIs without naming intent.

```rust
pub fn ok() -> () { () }
```

**GOOD** — semantic terminal value or explicit discard in pure pipelines.

```rust
use id_effect::foundation::unit::{discard, Unit};

fn consume_token(t: Token) -> Unit {
    discard(t)
}
```

### `never` / `absurd`

**BAD**

```rust
let r: Result<i32, Infallible> = Ok(1);
let v = r.expect("never fails"); // unnecessary panic path
```

**GOOD**

```rust
use id_effect::foundation::never::absurd;

let r: Result<i32, Infallible> = Ok(1);
let v = match r {
    Ok(x) => x,
    Err(e) => absurd(e),
};
```

### `function` vs `func` (prefer one import style per file)

**BAD** — mixing `effect::foundation::function::compose` and `effect::func::compose` in the same module.

```rust
use id_effect::foundation::function::compose as fcompose;
use id_effect::func::compose as gcompose;
```

**GOOD** — pick **`effect::func`** (crate re-export) for app code.

```rust
use id_effect::func::{compose, identity};
let f = compose(|x: i32| x + 1, |x: i32| x * 2);
```

### `product` (`fst`/`snd`/`pair`/…)

**BAD** — ad-hoc `.0`/`.1` with comments explaining order.

```rust
let p = (price, size);
let x = p.1; // size?
```

**GOOD** — projections with stable names.

```rust
use id_effect::foundation::product::{fst, snd};
let p = (price, size);
let _px = fst(p);
let _sz = snd(p);
```

### `coproduct` / `either` namespaces

**BAD** — `map_err`/`map` soup when a single elimination reads clearer.

```rust
let r = Ok(1).map_err(|e| e).map(|x| x + 1);
```

**GOOD** — categorical eliminator when branching both sides.

```rust
use id_effect::foundation::coproduct::{bimap, right};

let r = bimap(right(1), |l: String| l.len(), |r| r + 1);
```

### `isomorphism`

**BAD** — duplicated “parse/format” logic in two places without linking.

```rust
fn to_wire(x: i32) -> String { x.to_string() }
fn from_wire(s: &str) -> i32 { s.parse().unwrap() }
```

**GOOD** — package as `Iso` + round-trip tests.

```rust
use id_effect::foundation::isomorphism::Iso;

let iso = Iso::new(|x: i32| x.to_string(), |s: String| s.parse().unwrap_or(0));
```

### `option_::option`

**BAD** — nested `if let` chains for optional zip.

```rust
if let Some(a) = oa {
    if let Some(b) = ob {
        let _ = (a, b);
    }
}
```

**GOOD**

```rust
use id_effect::foundation::option_::option;
let z = option::zip(oa, ob);
```

### `piping` (`Pipe`)

**BAD** — deeply nested calls.

```rust
let y = f(g(h(x)));
```

**GOOD**

```rust
use id_effect::Pipe;
let y = x.pipe(h).pipe(g).pipe(f);
```

### `predicate`

**BAD** — boolean soup in business conditionals.

```rust
if x > 0 && x < 10 && x % 2 == 0 { /* ... */ }
```

**GOOD** — reusable, named predicates.

```rust
use id_effect::foundation::predicate::predicate;

let p = predicate::and(
    Box::new(|n: &i32| *n > 0) as effect::Predicate<i32>,
    Box::new(|n: &i32| *n < 10) as effect::Predicate<i32>,
);
```

### `mutable_ref`

**BAD** — hidden global mutable cell inside a library graph.

```rust
static CELL: MutableRef<i32> = MutableRef::make(0); // not actually static initializer safe; also wrong pattern
```

**GOOD** — keep shared state in `FiberRef`/`TRef`/services; use `MutableRef` only at a thin sync boundary if at all.

```rust
// At integration edge only:
let cell = MutableRef::make(0_i32);
cell.update(|n| *n += 1);
```

---

## `algebra` — laws and generic structure

**BAD** — reimplementing `map`/`flat_map` by hand on a custom wrapper everywhere.

```rust
impl<W> MyBox<W> {
    fn map<U>(self, f: impl FnOnce(W) -> U) -> MyBox<U> { MyBox(f(self.0)) }
}
```

**GOOD** — implement `Functor`/`Monad` once and reuse; **application** code usually uses `Effect` directly.

```rust
use id_effect::algebra::functor::Functor;
// Functor::map(&my_effect_like_type, f) in generic utilities/tests — not typical app code
```

---

## `kernel` — `Effect`, `IntoBind`, `from_async`, scopes

**BAD** — constructing effects with ad-hoc `async` blocks everywhere instead of `effect!`.

```rust
Effect::new_async(|r| box_future(async move {
    let _ = r;
    Ok(1)
}))
```

**GOOD**

```rust
effect!(|_r: &mut R| {
    1
})
```

**BAD** — leaking `BoxFuture` types at every call site.

```rust
pub fn f<'a, R: 'a>() -> BoxFuture<'a, Result<i32, ()>> { ... }
```

**GOOD** — return `Effect<i32, (), R>` and let combinators hide the future.

---

## `context` — tags, `Cons`/`Nil`, `Get`, optics, matchers

**BAD** — stringly-typed service lookup.

```rust
let log = env.get("logger").unwrap();
```

**GOOD** — type-keyed services.

```rust
let log = ~EffectLogger;
```

**BAD** — duplicating `zoom_env` closures many times.

```rust
eff.zoom_env(|outer| &outer.inner.field)
```

**GOOD** — reusable `EnvLens` for the same projection.

```rust
use id_effect::context::optics::{focus, EnvLens};
let lens = EnvLens::new(|outer: &Outer| &outer.inner.field);
let _ = focus(lens, eff);
```

**BAD** — giant `match` on strings scattered across modules.

```rust
match msg.type_id {
    "a" => handle_a(msg),
    "b" => handle_b(msg),
    _ => default(msg),
}
```

**GOOD** — `Matcher` with ordered predicates / tags (`context::match_`).

```rust
use id_effect::context::match_::Matcher;
use id_effect::foundation::predicate::Predicate;

let m = Matcher::new()
    .when(Box::new(|m: &Msg| m.tag() == "a") as Predicate<Msg>, handle_a)
    .or_else(|m| default(m));
```

---

## `layer` — `Layer`, stacks, graphs, `Service`

**BAD** — manually building a wrong `Cons` order repeatedly in every test.

```rust
let env = Context::new(Cons(svc_b, Cons(svc_a, Nil))); // easy to invert requirements
```

**GOOD** — `Layer::stack` / `merge_all` / graph planner (`LayerGraph`) for declared deps.

```rust
let env = merge_all(vec![layer_a(), layer_b()]).build().unwrap();
```

**BAD** — passing concrete client structs through every constructor.

```rust
struct App { db: DbClient }
impl App {
    fn job(&self) { let _ = &self.db; }
}
```

**GOOD** — services resolved from `R` via `NeedsDb`.

---

## `macros` — `pipe!`, `ctx!`, `service_key!`, graph macros

**BAD** — manual `Cons`/`Tagged` noise when `ctx!` exists.

```rust
let ctx = Context::new(Cons(Tagged::<K, _>::new(v), Nil));
```

**GOOD**

```rust
use id_effect::{ctx, Context, Cons, Nil, Tagged};

let ctx: Context<Cons<Tagged<K, V>, Nil>> = Context::new(Cons(Tagged::<K, _>::new(v), Nil));
// Or use `ctx!` where your codebase already does — keep one style project-wide.
```

**BAD** — using `pipe!` for effectful steps that need services.

```rust
pipe!(pure(1), |n| succeed(n + 1)) // loses access to R services unless wrapped carefully
```

**GOOD** — `effect!` for anything touching `~Service`.

---

## `runtime` — `run_blocking`, `run_async`, `Runtime`, `run_fork`

**BAD**

```rust
fn deep_inside_lib() {
    let _ = run_blocking(work(), env());
}
```

**GOOD**

```rust
fn main() {
    let _ = run_blocking(work(), env());
}
```

**BAD** — ignoring `Never` channels with `unwrap`.

```rust
let r: Result<i32, Never> = Ok(1);
let _ = r.unwrap();
```

**GOOD** — eliminate `Err` with `absurd` or use helpers that strip `Never`.

---

## `concurrency` — fibers, cancellation, `FiberRef`

**BAD** — `std::thread::spawn` + ad-hoc channels for everything Effect already models.

```rust
std::thread::spawn(|| {
    // unsupervised work
});
```

**GOOD** — `run_fork` / `FiberHandle` with structured join/cancel.

```rust
let h = run_fork(rt, || (work(), env()));
// join / interrupt via FiberHandle API
```

**BAD** — thread-local globals for request context.

```rust
thread_local! { static X: Cell<u64> = const { Cell::new(0) }; }
```

**GOOD** — `FiberRef` for fiber-scoped dynamic state.

```rust
let _ = with_fiber_id(id, || work());
```

---

## `coordination` — channels, queues, semaphores, pubsub, refs

**BAD** — unbounded `std::sync::mpsc` without backpressure policy in streaming pipelines.

```rust
let (tx, rx) = std::sync::mpsc::channel();
```

**GOOD** — `Queue` / `Channel` + `Stream` policies (`BackpressurePolicy`) where appropriate.

```rust
let (s, recv) = stream_from_channel_with_policy(cap, policy);
```

**BAD** — `Rc<RefCell<T>>` shared across fibers without runtime integration.

```rust
let r = Rc::new(RefCell::new(0));
```

**GOOD** — `Ref`/`SynchronizedRef` from `coordination` when you must share cells through the effect runtime.

---

## `failure` — `Cause`, `Exit`, `Or`

**BAD** — flattening all errors to `String` at fiber boundaries.

```rust
Err(format!("{e:?}"))
```

**GOOD** — structured `Cause` / `Exit` preserving diagnostics.

```rust
Exit::fail(Cause::from("db"))
```

**BAD** — nested `Result<Result<…>>` types in public APIs.

```rust
type T = Result<Result<i32, E1>, E2>;
```

**GOOD** — `Or<E1, E2>` or a single `E: From<…>` channel.

---

## `resource` — `Scope`, pools, caches

**BAD** — manual close in happy path only.

```rust
let c = connect();
let _ = work(c);
// forgot: c.close()
```

**GOOD** — `Scope` finalizers.

```rust
scope.acquire(c, |conn| work(conn))
```

**BAD** — unbounded `Cache` without stats or eviction strategy in long-running processes.

```rust
let mut cache = HashMap::new();
```

**GOOD** — `Cache` with explicit stats/metrics hooks when using `effect::resource::Cache`.

---

## `scheduling` — durations, clocks, retries

**BAD** — `std::thread::sleep` in domain code.

```rust
std::thread::sleep(Duration::from_secs(1));
```

**GOOD** — inject `Clock` / use schedule combinators so tests can advance time (`TestClock`).

```rust
retry_with_clock(policy, || work(), &clock)
```

**BAD** — hard-coded UTC/Local assumptions in strategy code.

```rust
let now = std::time::SystemTime::now();
```

**GOOD** — `UtcDateTime` / `ZonedDateTime` with explicit zones (`timezone` helpers).

---

## `observability` — metrics & tracing hooks

**BAD** — unstructured `println!` inside hot effects.

```rust
println!("here");
```

**GOOD** — `with_span` / `emit_effect_event` integration points (wire real exporters at the edge).

```rust
with_span("step", || work())
```

---

## `schema` — brands, parse pipeline, errors, serde bridge

**BAD** — `serde_json::from_value` deep in domain without validation story.

```rust
let x: MyDto = serde_json::from_value(v).unwrap();
```

**GOOD** — parse `Unknown` via `Schema` to typed values; accumulate with `ParseErrors`.

```rust
let u = Unknown::from_json(v);
let _ = struct_(&[("f", string())]).parse(u)?;
```

**BAD** — using raw `i64` for IDs that must be positive.

```rust
fn order_id(x: i64) { let _ = x; }
```

**GOOD** — `Brand` / refined types.

```rust
type OrderId = Brand<i64, OrderMarker>;
```

**BAD** — ad-hoc JSON equality in tests.

```rust
assert_eq!(a.to_string(), b.to_string());
```

**GOOD** — `wire_equal` / `Equal` when comparing schema-shaped values.

---

## `stm` — `Stm`, `TRef`, `commit`

**BAD** — mixing arbitrary `Mutex`+manual lock ordering across modules.

```rust
let a = Mutex::new(1);
let b = Mutex::new(2);
let _ = (a.lock(), b.lock());
```

**GOOD** — `Stm` transactions for related cells; lift with `commit`/`atomically`.

```rust
run_blocking(commit(tref.read_stm().flat_map(|v| /* ... */)), ())
```

**BAD** — calling `Txn` directly in app code.

```rust
let mut txn = Txn::new();
```

**GOOD** — build `Stm` programs with combinators; let `commit` run them.

---

## `streaming` — `Chunk`, `Sink`, `Stream`

**BAD** — eager `Vec::collect` of an unbounded event source.

```rust
let all: Vec<_> = source.collect();
```

**GOOD** — `Stream` with chunking/backpressure.

```rust
stream
    .map(|x| x * 2)
    .run_collect_effect(...)
```

**BAD** — ignoring `BackpressurePolicy` when bridging to channels.

```rust
let _ = stream_from_channel(rx); // policy matters under load
```

**GOOD**

```rust
let _ = stream_from_channel_with_policy(rx, BackpressurePolicy::DropLatest);
```

---

## `collections` — persistent vs mutable structures

**BAD** — cloning giant `HashMap` on every small update in a hot loop.

```rust
let mut m = map.clone();
m.insert(k, v);
```

**GOOD** — `EffectHashMap` persistent updates when structural sharing wins.

```rust
let m2 = m.update(k, v);
```

**BAD** — using `MutableList` where `Vector` persistence is safer and clearer.

```rust
let ml = MutableList::new();
```

**GOOD** — pick **persistent** collections for pure snapshots; **mutable** mutex-backed ones only for shared streaming assembly.

---

## `testing` — `run_test`, clocks, leak checks

**BAD**

```rust
#[test]
fn t() {
    let _ = run_blocking(effect_that_spawns(), ());
}
```

**GOOD**

```rust
#[test]
fn t() {
    let _ = run_test(effect_that_spawns());
}
```

**BAD** — real time in unit tests.

```rust
std::thread::sleep(Duration::from_secs(2));
```

**GOOD**

```rust
run_test_with_clock(effect, |clk| {
    clk.advance(Duration::from_secs(2));
})
```

---

## Crate root conventions (`im` re-export)

**BAD** — pinning multiple `im` versions across crates.

```rust
// crate A: im 15
// crate B: im 16
```

**GOOD** — depend on `effect::im` / `effect::collections` aliases for a single version line.

```rust
use id_effect::im::Vector;
use id_effect::EffectVector;
```

---

## Streams (domain loops) — recap

**BAD** — manual `while` with mutable outer state that should be compositional.

```rust
let mut s = st;
loop {
    if done(&s) {
        break;
    }
    s = step(s);
}
```

**GOOD** — `Stream::unfold_effect` / folds when modeling lazy pipelines.

```rust
Stream::unfold_effect(init, |s| {
    effect!(|r: &mut R| {
        let out = ~step(s);
        Ok(out)
    })
})
```

---

## Service pattern — minimal recap

**BAD** — ambient globals.

```rust
static LOGGER: Logger = Logger;
```

**GOOD** — `service_key!`, `IntoBind`, `NeedsX`, `~MyService` inside `effect!` (see HARD rules).

---

## Typed errors — minimal recap

**BAD** — collapsing all service errors into domain via `impl From<ServiceErr> for DomainErr`.

```rust
impl From<LogErr> for DomainErr { fn from(_: LogErr) -> Self { Self::Any } }
```

**GOOD** — `E: From<DomainErr> + From<LogErr>` on the effect.

---

## Wiring at the top level — minimal recap

**BAD** — library builds env and runs.

**GOOD** — binary/test builds `Context`/`Layer` stack; calls `run_blocking`/`run_test`.

---

## Anti-patterns checklist (convert every row)

| BAD | GOOD |
|-----|------|
| `async fn` app fn | `fn … -> Effect<…>` + `from_async`/`effect!` |
| Concrete `R` in library API | generic `R: Needs…` |
| `fn f<E,R>(…) -> Effect<T,E,R>` | `fn f<A,E,R>(…) -> Effect<A,E,R>` + `A: From<T>` |
| Services as parameters | `~Tag` |
| Old postfix `~` | `let x = ~e;` / `~e;` |
| `run_blocking` in libs | return `Effect` |
| `_effect` suffix | operation name |
| `println!` tracing | spans / structured hooks |
| Hidden `MutableRef` globals | `FiberRef` / `TRef` / `R` services |

---

## Quick reference

| Topic | Idiomatic choice |
|------|------------------|
| Describe work | `Effect<A,E,R>` |
| Sequence | `effect!` with `~` |
| Third-party async | `~from_async` + `map_err` into `E` |
| Run | `run_blocking` / `run_async` / `run_test` |
| Services | `service_key!` + `NeedsX` + `~Svc` |
| Errors | `E: From<Domain>` + `fail(…).into()` |
| Time | `Clock` / `retry_with_clock` |
| Shared state in fibers | `FiberRef` / `TRef` / STM |
| Validation | `schema` parsers |
| Streaming | `Stream` + backpressure |
| Collections | persistent `EffectHashMap` / `EffectVector` when cloning snapshots |

---

## Final rule

**Every composable domain/library function that returns an effect is `fn …<A, E, R>(…) -> Effect<A, E, R>` with honest bounds.** Build the graph in one `effect!`, wire services at the top, run at the edge, parse wire formats at boundaries, and use the `effect` crate’s constructs for what they are for—**not** ad-hoc copies of the same ideas.

---

## Appendix — remaining `effect` constructs (each with BAD / GOOD)

The sections above cover the **big rocks**. The pairs below complete the surface area of `crates/id_effect/src/*` so no public concept is left without guidance.

### `algebra::semigroup` / `monoid`

**BAD** — ad-hoc “combine” that forgets empty.

```rust
fn combine(a: Metrics, b: Metrics) -> Metrics { /* … */ }
fn empty() -> Metrics { Metrics::default() } // not wired as monoid laws
```

**GOOD** — explicit `Semigroup`/`Monoid` instances when writing generic folds or tests over laws.

```rust
use id_effect::algebra::monoid::Monoid;
// impl Monoid for Metrics where combine/empty obey laws — use in generic algorithms
```

### `algebra::selective` / `applicative` / `contravariant` / `bifunctor` / `interface`

**BAD** — using these traits in everyday app services “because FP”.

```rust
fn app_step(x: impl Applicative<...>) -> impl Applicative<...> { ... }
```

**GOOD** — reserve for generic utilities, law tests, or rare abstractions; **prefer `Effect`’s concrete API** in application layers.

### `kernel::thunk` / `kernel::result` / `kernel::reader`

**BAD** — importing kernel modules in app crates to reinvent `Effect`.

```rust
use id_effect::kernel::thunk::Thunk;
```

**GOOD** — use **`Effect` + `effect!`**; touch kernel submodules only when extending the runtime or writing advanced library code inside `effect` itself.

### `context` paths (`Here`, `Skip1`, …) and `prepend_cell`

**BAD** — using `Skip3` without types guiding correctness.

```rust
ctx.get_path::<MyKey, Skip3>() // broke when stack changed
```

**GOOD** — prefer `NeedsX` at boundaries; use explicit paths only where the stack is fixed and tested.

```rust
let _ = ctx.get::<MyKey>(); // head — stable when key is unique at head
```

### `layer::graph` (`LayerGraph`, `LayerNode`, `LayerPlan`)

**BAD** — hard-coded topo order in comments across three files.

```rust
// 1) build A 2) build B 3) build C
```

**GOOD** — declare `requires`/`provides` and let the planner produce a `LayerPlan`.

### `macros` — `service_def`, `layer_graph!`, `layer_node!`, `req!`, `err!`

**BAD** — copy-pasting large `Cons` trees for every new service.

```rust
type R = Cons<..., Cons<..., Nil>>;
```

**GOOD** — generate keys/defs with `service_key!` / `service_def` patterns already used in-repo; use graph macros when the dependency DAG grows.

### `coordination::Deferred` / `Latch` / `PubSub`

**BAD** — ad-hoc `Condvar` + `Mutex<bool>` for “signal once”.

```rust
let pair = (Mutex::new(false), Condvar::new());
```

**GOOD** — `Deferred`/`Latch` for structured one-shot / barrier synchronization integrated with fiber lifetimes.

### `coordination::Queue` vs `channel::Channel`

**BAD** — mixing std channels and effect channels randomly in one pipeline.

```rust
let (tx, rx) = std::sync::mpsc::channel();
```

**GOOD** — pick **`Queue`/`Channel`** consistently when integrating with `Stream` and effect runtime semantics.

### `schema::data` (`DataStruct`, `DataTuple`, `DataError`)

**BAD** — untyped `Vec<Unknown>` as your only runtime model.

```rust
let row: Vec<Unknown> = vec![];
```

**GOOD** — use `DataStruct`/`DataTuple` to give rows predictable shape when driving generic tools (charts, exporters).

### `schema::equal` / `EffectHash`

**BAD** — hashing via `format!("{:?}", x)` for composite keys.

```rust
let h = format!("{:?}", key);
```

**GOOD** — `EffectHash` / `hash_structure` for stable structural hashing.

### `schema::order` (`DynOrder`, `ordering`)

**BAD** — sorting with `f64` keys without handling NaN/order rules.

```rust
xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
```

**GOOD** — explicit `DynOrder` / total order policy for schema-driven sorts.

### `schema::parse` primitives (`i64`, `string`, `union_`, `optional`, `refine`, …)

**BAD** — `as i64` casts on JSON numbers.

```rust
let x = v.as_i64().unwrap();
```

**GOOD** — `i64()` / `refine` / `filter` schema steps that accumulate `ParseErrors`.

### `schema::extra` (`record`, `suspend`, `union_chain`, `literal_*`, `null_or`, `wire_equal`)

**BAD** — open-coded `serde_json::Value` matches for unions.

```rust
match v.get("type") { Some(_) => ..., None => ... }
```

**GOOD** — `union_chain` / `record` combinators matching your wire conventions.

### `schema::parse_errors` (`ParseErrors`)

**BAD** — returning first parse error only in batch imports.

```rust
if let Err(e) = parse_one(&x[0]) { return Err(e); }
```

**GOOD** — accumulate with `ParseErrors` for operator-grade diagnostics.

### `schema::has_schema` (`HasSchema`)

**BAD** — duplicating schema metadata in README and code separately.

```rust
// docs say fields; code drifts
```

**GOOD** — attach `HasSchema` to types that must round-trip documentation/tooling.

### `schema::json_schema_export` / `serde_bridge` (feature `schema-serde`)

**BAD** — hand-maintaining JSON Schema snippets.

```json
{ "type": "object" }
```

**GOOD** — export fragments from the same combinators (`type_string`, `type_record`, …) and parse `serde_json` via `unknown_from_serde_json` when bridging ecosystems.

### `stm::TQueue` / `TMap` / `TSemaphore`

**BAD** — mixing `std::collections::VecDeque` + locks for transactional patterns.

```rust
let q = Mutex::new(VecDeque::new());
```

**GOOD** — `TQueue`/`TMap`/`TSemaphore` inside `Stm` programs; commit with `commit`/`atomically`.

### `streaming::Chunk` / `Sink`

**BAD** — passing raw `Vec<T>` everywhere without batch semantics.

```rust
fn send_all(items: Vec<T>) { /* huge alloc */ }
```

**GOOD** — `Chunk` + `Sink` to process incremental batches with explicit lifecycle.

### `streaming::StreamSender` / fanout helpers

**BAD** — cloning a channel sender to N tasks without policy.

```rust
for _ in 0..N { let tx = tx.clone(); /* spawn */ }
```

**GOOD** — `StreamBroadcastFanout` / disciplined `stream_from_channel*` setup with a chosen `BackpressurePolicy`.

### `collections::EffectSortedMap` / `EffectSortedSet`

**BAD** — sorting a `Vec` of keys on every lookup.

```rust
keys.sort();
```

**GOOD** — ordered maps/sets when you need deterministic iteration + log-time lookup.

### `collections::RedBlackTree` / `Trie`

**BAD** — flattening hierarchical keys (`"a/b/c"`) into strings without structure.

```rust
let k = format!("{a}/{b}/{c}");
```

**GOOD** — `Trie` for prefix walks; `RedBlackTree` when you need multimaps/order queries (see module docs).

### `collections::MutableQueue` / `MutableList` (`ChunkBuilder`)

**BAD** — `Vec` + `Mutex` for “append-only shared log” in fiber-heavy code.

```rust
let v = Mutex::new(Vec::new());
```

**GOOD** — `MutableList`/`ChunkBuilder` when assembling chunks for streaming sinks.

### `observability::metric` (`metric_make`)

**BAD** — ad-hoc global atomics per metric scattered in modules.

```rust
static C: AtomicU64 = AtomicU64::new(0);
```

**GOOD** — `Metric` via `metric_make` so effect execution and fiber context can correlate events consistently.

### `observability::tracing` (`TracingFiberRefs`, `emit_fiber_event`, …)

**BAD** — printing fiber id manually on every log line.

```rust
println!("fiber={id} …");
```

**GOOD** — `emit_fiber_event` / snapshot APIs that integrate with the tracing model.

### `testing::snapshot` (`SnapshotAssertion`)

**BAD** — huge `assert_eq!` on strings that change whitespace-only.

```rust
assert_eq!(json, "{ \"a\": 1 }");
```

**GOOD** — snapshot assertions (module helpers) for stable golden outputs of integrated effect runs.

### Crate root: `extern crate self as id_effect`

**BAD** — macros emitting unstable relative paths.

```rust
// proc macro outputs `use crate::Effect` from consumer crate — breaks
```

**GOOD** — rely on `::id_effect::…` paths from macros (`extern crate self as id_effect` is why this works inside the `id_effect` crate).

---

### Construct checklist (coverage index — not paired examples)

If you introduce behavior that corresponds to a row below, **use the `effect` type**—do not reintroduce a parallel pattern.

| Area | Constructs |
|------|-------------|
| **foundation** | `unit`, `never`, `function`/`func`, `product`, `coproduct`/`either`, `isomorphism`, `option_`, `Pipe`, `Predicate`, `MutableRef` |
| **algebra** | `Semigroup`, `Monoid`, `Functor`, `Bifunctor`, `Contravariant`, `Applicative`, `Monad`, `Selective`, `interface` |
| **kernel** | `Effect`, `BoxFuture`, `IntoBind`, `succeed`/`fail`/`pure`/`from_async`, `scoped`/`scope_with`/`acquire_release`, `thunk`/`result`/`reader` |
| **context** | `Tag`, `Tagged`, `Cons`/`Nil`, `Context`, `Get`/`GetMut`, paths, `Matcher`/`HasTag`, `EnvLens`/`focus` |
| **layer** | `Layer`, `Stack`/`StackThen`, `LayerGraph`/`LayerNode`/`LayerPlan`, `Service`/`ServiceEnv`, `provide_service` |
| **macros** | `effect!`, `pipe!`, `ctx!`, `service_key!`, `service_def`, `layer_graph!`, `layer_node!`, `req!`, `err!` |
| **runtime** | `run_blocking`, `run_async`, `run_fork`, `yield_now`, `Runtime`, `Never` |
| **concurrency** | `FiberId`, `FiberHandle`/`FiberStatus`, `CancellationToken`, `FiberRef` |
| **coordination** | `Deferred`, `Latch`, `Queue`, `Semaphore`/`Permit`, `PubSub`, `Channel`, `Ref`, `SynchronizedRef` |
| **failure** | `Cause`, `Exit`, `Or` |
| **resource** | `Scope`/`Finalizer`, `Pool`/`KeyedPool`, `Cache`/`CacheStats` |
| **scheduling** | `Duration`, datetimes, `Clock`/`TestClock`, `Schedule`, `repeat*`/`retry*` |
| **observability** | `Metric`, tracing hooks / snapshots |
| **schema** | `Brand`, `Equal`/`EffectHash`, `EffectData`, `Ordering`/`DynOrder`, `Schema`/`Unknown`/`ParseError(s)`, `HasSchema`, serde bridge (feature) |
| **stm** | `Stm`, `Outcome`, `commit`/`atomically`, `TRef`, `TQueue`, `TMap`, `TSemaphore` |
| **streaming** | `Chunk`, `Sink`, `Stream`, policies, channel bridging |
| **collections** | `EffectHashMap`/`Set`, sorted map/set, `EffectVector`, `RedBlackTree`, `MutableList`/`ChunkBuilder`, `MutableQueue`, `Trie` |
| **testing** | `run_test`, `run_test_with_clock`, leak/scope checks, snapshots |
