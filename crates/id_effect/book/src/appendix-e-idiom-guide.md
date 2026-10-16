# Idiom and Style Guide for Application Code

This is the house style for code that *uses* `id_effect`: services, handlers, jobs and their
tests. Each rule is one line, followed by a snippet that compiles and runs against the current
crate (the snippets on this page are doctests of `id_effect`, so `cargo test -p id_effect --doc`
fails if one drifts). The chapters linked from each section explain the machinery.

The running example is a small `orders` service.

## 1. When to use `effect!`

- **One step:** return the constructor or combinator directly (`succeed`, `fail`, `.map`,
  `.map_err`). No macro.
- **Two or more dependent steps:** one `effect!({ .. })` per function, with the full return type
  `Effect<A, E, caps!(..)>` written on the function. The macro takes `E` and `R` from that type.
- **Independent steps:** join them, `~(a, b)` inside the macro or `zip_par(a, b)` outside it.
- **Borrowed arguments:** build the sub-effect *before* the macro (`let parsed = parse_id(raw);`)
  and bind it inside; the macro body is `'static` and cannot hold the borrow.
- Use `?` on a `Result` inside the body to fail with its error; do not nest `effect!` inside
  `effect!`.

```rust
use id_effect::{Effect, Exit, caps, effect, fail, succeed};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OrderId(u64);

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum OrderError {
  BadId(String),
  NotFound(u64),
}

/// Known order ids (a capability).
pub struct Catalog(Vec<u64>);

// One step: no macro.
fn parse_id(raw: &str) -> Effect<OrderId, OrderError> {
  match raw.parse::<u64>() {
    Ok(n) => succeed(OrderId(n)),
    Err(_) => fail(OrderError::BadId(raw.to_owned())),
  }
}

// Several steps: one `effect!`, the return type spelled out.
fn find_order(raw: &str) -> Effect<OrderId, OrderError, caps!(Catalog)> {
  let parsed = parse_id(raw); // built outside: `raw` is borrowed
  effect!({
    let id = ~parsed;
    let catalog = ~Catalog;
    if !catalog.0.contains(&id.0) {
      Err::<(), _>(OrderError::NotFound(id.0))?;
    }
    id
  })
}

let catalog = || Catalog(vec![7]);
assert_eq!(find_order("7").provide(catalog()).block_on(), Exit::succeed(OrderId(7)));
assert!(find_order("8").provide(catalog()).block_on().into_result().is_err());
assert!(find_order("x").provide(catalog()).block_on().into_result().is_err());
```

See [The effect! Macro](./part1/ch03-00-effect-macro.md) and
[When Not to Use the Macro](./part1/ch03-04-when-not-to-use.md).

## 2. Error modelling

- **One error enum per boundary** (a service, a use case), with a variant per failure a caller
  can act on. Errors are values: derive `Debug`, and `PartialEq, Eq, Hash` so tests can compare
  `Exit`s (`Exit` and `Cause` equality requires all three of the value and the error).
- **`impl From<Inner> for Outer`** for each lower-level error. `~sub_effect` inside `effect!`
  converts with `From` at the bind, so there is no `map_err` per call.
- **`.map_err`** only where there is no `From` (or where a `From` would be ambiguous), and
  **`.catch`** to recover from a typed error.
- **Defects are for bugs**, not for domain failures: `die(..)` (or a panic inside the effect)
  ends the fiber with `Cause::Die`, outside the typed `E`.
- For a one-off pair of errors, `err!(A | B)` spells `Or<A, B>`; `Or` converts from its left arm
  only, so prefer a named enum once a third case appears.

```rust
use id_effect::{Cause, Effect, Exit, effect, fail, succeed};

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum DbError {
  Timeout,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub enum PaymentError {
  Declined,
}

/// The checkout use case's error: one variant per lower-level error.
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum CheckoutError {
  Db(DbError),
  Payment(PaymentError),
}

impl From<DbError> for CheckoutError {
  fn from(e: DbError) -> Self {
    Self::Db(e)
  }
}

impl From<PaymentError> for CheckoutError {
  fn from(e: PaymentError) -> Self {
    Self::Payment(e)
  }
}

fn load_total(order: u64) -> Effect<u64, DbError> {
  if order == 0 { fail(DbError::Timeout) } else { succeed(order * 100) }
}

fn charge(cents: u64) -> Effect<u64, PaymentError> {
  if cents > 10_000 { fail(PaymentError::Declined) } else { succeed(cents) }
}

fn checkout(order: u64) -> Effect<u64, CheckoutError> {
  effect!({
    let total = ~load_total(order); // DbError -> CheckoutError via From
    ~charge(total) // PaymentError -> CheckoutError via From
  })
}

assert_eq!(checkout(3).block_on(), Exit::succeed(300));
assert_eq!(
  checkout(0).block_on(),
  Exit::Failure(Cause::Fail(CheckoutError::Db(DbError::Timeout)))
);

// Recover from one variant, keep the others.
let recovered = checkout(0).catch(|e| match e {
  CheckoutError::Db(_) => succeed(0),
  other => fail(other),
});
assert_eq!(recovered.block_on(), Exit::succeed(0));
```

See [Error Handling](./part3/ch08-00-error-handling.md).

## 3. Layer vs provide

- **Business functions name what they need** in `R` (`caps!(Db)`) and never construct it.
- **`.provide(value)`** when you already hold the value: in tests, and for plain values at the
  edge. It removes exactly that capability from `R`.
- **A `Layer`** when building the capability is itself an effect, needs other capabilities,
  owns a resource (`Layer::acquire_release`, `Layer::scoped`) or is shared: a layer used twice
  in one build is built once. Compose layers with `merge` / `provide_to` / `provide_merge`.
- **`.provide_layer(layer)` once, at the edge** (`main`, the request handler), never deep in the
  call graph. The layer's error type is the effect's error type.

```rust
use id_effect::{Effect, Exit, HCons, HNil, Layer, caps, effect, hlist, need};

#[derive(Clone)]
pub struct Config {
  db_url: &'static str,
}

pub struct Db {
  url: String,
}

// Business code: names `Db`, does not build it.
fn describe_store() -> Effect<String, String, caps!(Db)> {
  effect!({
    let db = ~Db;
    format!("orders in {}", db.url)
  })
}

// Construction that needs another capability: a layer.
fn db_layer() -> Layer<HCons<Db, HNil>, String, HCons<Config, HNil>> {
  Layer::effect(|| need::<Config>().map(|c| hlist![Db { url: c.db_url.to_owned() }]))
}

// The edge: wire the graph once and provide it.
let app = Layer::succeed(hlist![Config { db_url: "pg://orders" }]).provide_to(db_layer());
assert_eq!(
  describe_store().provide_layer(app).block_on(),
  Exit::succeed("orders in pg://orders".to_owned())
);

// A test: provide the value directly.
let fake = Db { url: "mem://".to_owned() };
assert_eq!(
  describe_store().provide(fake).block_on(),
  Exit::succeed("orders in mem://".to_owned())
);
```

See [Layers](./part2/ch06-00-layers.md) and [Services](./part2/ch07-00-services.md).

## 4. Retry factories

- **`Effect::retry(make, schedule)`** with `make: Fn() -> Effect<..>`. Effects are one-shot
  values, so retry takes a *factory* that builds a fresh attempt each time; never write the
  retry loop by hand.
- **Put the per-attempt timeout inside the factory** (`.timeout_fail(d, || err)`), so each
  attempt gets its own deadline; put an overall timeout outside the `retry`.
- **Bound every schedule**: `Schedule::exponential(base).intersect(Schedule::recurs(n))` stops
  after `n` retries; an unbounded schedule retries forever.
- Delays run on the executor's clock, so the same code is tested on virtual time
  ([section 6](#6-testing)).

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use id_effect::{Effect, Exit, Schedule, fail, succeed, suspend};

/// Fails twice, then returns the rate. `suspend` defers the check until the attempt runs.
fn fetch_rate(calls: Arc<AtomicU32>) -> Effect<u32, String> {
  suspend(move || {
    let n = calls.fetch_add(1, Ordering::SeqCst);
    if n < 2 { fail(format!("attempt {n} failed")) } else { succeed(42) }
  })
}

let calls = Arc::new(AtomicU32::new(0));
let make = {
  let calls = Arc::clone(&calls);
  move || {
    fetch_rate(Arc::clone(&calls))
      .timeout_fail(Duration::from_secs(2), || "attempt timed out".to_owned())
  }
};
let policy = Schedule::exponential(Duration::from_millis(1)).intersect(Schedule::recurs(3));

assert_eq!(Effect::retry(make, policy).block_on(), Exit::succeed(42));
assert_eq!(calls.load(Ordering::SeqCst), 3); // 1 attempt + 2 retries
```

See [retry and repeat](./part3/ch11-03-retry-repeat.md).

## 5. Resource scopes

- **A resource used by one effect:** `acquire_use_release(acquire, use, release)`. `acquire`
  is uninterruptible and `release` runs on success, failure, defect and interrupt; it receives
  the `ExitKind`.
- **Several resources or finalizers for one block:** `Effect::scoped(|scope| ..)` with
  `scope.add_finalizer(..)`; finalizers run when the block ends.
- **Cleanup that is not a resource** (a metric, a log line): `.ensuring(finalizer)`.
- **A resource that lives as long as the application** (a pool, a client): a layer,
  `Layer::acquire_release(acquire, release)`, provided at the edge.
- Finalizers are `Effect<(), Never, ..>`: they cannot fail, so handle their errors inside them.

```rust
use std::sync::{Arc, Mutex};
use id_effect::{Effect, Exit, acquire_use_release, fail, succeed, sync};

#[derive(Clone)]
pub struct Conn {
  id: u32,
  log: Arc<Mutex<Vec<String>>>,
}

fn with_conn(log: &Arc<Mutex<Vec<String>>>, ok: bool) -> Effect<u32, String> {
  let log = Arc::clone(log);
  acquire_use_release(
    sync(move || Conn { id: 1, log }),
    move |conn| if ok { succeed(conn.id * 10) } else { fail("query failed".to_owned()) },
    |conn, kind| {
      sync(move || conn.log.lock().expect("log").push(format!("closed {} ({kind:?})", conn.id)))
    },
  )
}

let log = Arc::new(Mutex::new(Vec::new()));
assert_eq!(with_conn(&log, true).block_on(), Exit::succeed(10));
assert!(with_conn(&log, false).block_on().into_result().is_err());
assert_eq!(*log.lock().expect("log"), ["closed 1 (Success)", "closed 1 (Failure)"]);

// A block with its own finalizer.
let closed = Arc::new(Mutex::new(false));
let flag = Arc::clone(&closed);
let scoped: Effect<u32, String> = Effect::scoped(move |scope| {
  scope
    .add_finalizer(sync(move || *flag.lock().expect("flag") = true))
    .map(|()| 7)
});
assert_eq!(scoped.block_on(), Exit::succeed(7));
assert!(*closed.lock().expect("flag"));
```

See [Resources & Scopes](./part3/ch10-00-resources.md).

## 6. Testing

- **Fake capabilities with `.provide(fake)`**; test business functions, not layers, unless the
  wiring is the subject.
- **Time:** run the effect on a `TestExecutor` (`eff.fork_root(Arc::new(exec.clone()))`) and
  drive it with `run_until_stalled` and `advance`; sleeps, timeouts and retry delays take no
  real time.
- **Randomness:** name `Random` in `R` and provide `TestRandom::seeded(..).random()`, feeding
  values with `feed_u64s` when a test needs exact draws.
- **Concurrency:** run fiber-interleaving tests under `explore_interleavings(seeds, test)` or
  `assert_interleavings`; a failure names its seed, and `TestExecutor::seeded(seed)` replays it
  ([Exploring Interleavings](./part4/ch15-05-interleavings.md)).
- Assert on the `Exit` (`Exit::succeed(..)`, `Exit::Failure(Cause::Fail(..))`), not on logs.

```rust
use std::sync::Arc;
use std::time::Duration;
use id_effect::{Effect, Schedule, TestExecutor, fail};

// The retry policy from section 4, on virtual time: 10 + 20 + 40 ms of backoff.
let always_down = || fail::<u32, String, _>("down".to_owned());
let policy = Schedule::exponential(Duration::from_millis(10)).intersect(Schedule::recurs(3));
let eff: Effect<u32, String> = Effect::retry(always_down, policy);

let exec = TestExecutor::new();
let h = eff.fork_root(Arc::new(exec.clone()));
exec.run_until_stalled();
exec.advance(Duration::from_millis(69));
assert!(!h.is_done()); // still backing off
exec.advance(Duration::from_millis(1));
assert!(h.is_done()); // all three retries spent, after exactly 70 ms of virtual time
```

See [Testing](./part4/ch15-00-testing.md).

## Summary

| Situation | Idiom |
|-----------|-------|
| 1 step | constructor or `.map` / `.map_err`, no macro |
| 2+ dependent steps | one `effect!` per function, return type spelled out |
| Independent steps | `~(a, b)`, `zip_par`, `all`, `for_each_par` |
| Lower-level error | `From` impl into the boundary's enum; `~` converts |
| Bug, not a domain failure | `die(..)` |
| Value at hand (tests, plain config) | `.provide(value)` |
| Built, dependent, shared or resourceful capability | `Layer`, `.provide_layer` at the edge |
| Retry | `Effect::retry(factory, bounded schedule)`; per-attempt timeout in the factory |
| Resource for one effect | `acquire_use_release` |
| Resource for the app | `Layer::acquire_release` |
| Time, randomness, interleavings in tests | `TestExecutor`, `TestRandom`, `explore_interleavings` |
