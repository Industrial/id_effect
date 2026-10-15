# Scopes and Finalizers — Guaranteed Cleanup

A `Scope` is a finalizer registry. Cleanup effects registered in a scope run when the scope closes — however it closes.

## Effect::scoped

`Effect::scoped(|scope| body)` creates a fresh scope, runs the body with it, and closes the scope when the body ends (success, failure, defect or interrupt). Finalizers run in reverse order of registration (LIFO, like destructors). From `examples/091_scope_with.rs`:

```rust
use std::sync::{Arc, Mutex};
use id_effect::{Effect, Exit, succeed, sync};

let order = Arc::new(Mutex::new(Vec::new()));
let o = Arc::clone(&order);
let eff: Effect<u8, &'static str> = Effect::scoped(move |scope| {
    let o1 = Arc::clone(&o);
    let o2 = Arc::clone(&o);
    scope
        .add_finalizer(sync(move || o1.lock().unwrap().push(1_u8)))
        .then(scope.add_finalizer(sync(move || o2.lock().unwrap().push(2_u8))))
        .then(succeed(0))
});
assert_eq!(eff.block_on(), Exit::succeed(0));
assert_eq!(*order.lock().unwrap(), vec![2, 1]); // last registered runs first
```

A finalizer is an `Effect<(), Never, HNil>`: it cannot fail and needs no capabilities (capture what it needs). Registration itself is an effect — it happens when `add_finalizer(..)` runs, not when you call the method.

## Finalizers Always Run

```rust,ignore
let result = Effect::scoped(move |scope| effect!({
    let conn = ~open_connection();
    ~scope.add_finalizer(close_connection(conn.clone()));
    ~risky_operation(&conn) // may fail, panic, or be interrupted
}));
// However risky_operation ends, close_connection(conn) has run before `result` completes.
```

The body keeps the caller's interruptibility; closing the scope is uninterruptible. A defect in a finalizer is sequenced after the body's outcome (`Cause::Then`), or replaces a success.

## Knowing How the Scope Closed

`add_finalizer_exit(|kind| ..)` receives the closing `ExitKind` (`Success`, `Failure`, `Die` or `Interrupt`) — commit on success, roll back otherwise:

```rust,ignore
~scope.add_finalizer_exit(move |kind| match kind {
    ExitKind::Success => commit(txn),
    _ => rollback(txn),
});
```

## ensuring: One Finalizer, One Effect

When you only need "run this afterwards, whatever happens", skip the scope:

```rust
use std::sync::{Arc, Mutex};
use id_effect::{Effect, Never, fail, sync};

let calls = Arc::new(Mutex::new(0usize));
let c = Arc::clone(&calls);
let fin: Effect<(), Never> = sync(move || *c.lock().unwrap() += 1);

let err: Effect<u8, &'static str> = fail("err");
assert!(err.ensuring(fin).block_on().into_result().is_err());
assert_eq!(*calls.lock().unwrap(), 1);
```

## Scopes You Manage Yourself

`Scope::new()` is a plain constructor. Pass `&scope` to APIs that check things out into it (`Pool::get`, `PubSub::subscribe`, `Semaphore::acquire_scoped`), fork fibers into it with `scope.fork_in(eff)` (they are interrupted when it closes), and close it explicitly with `scope.close(ExitKind::Success)`. Closing an already closed scope is a no-op; registering into a closed scope runs the finalizer at once.

Scopes nest naturally: an inner `Effect::scoped` closes (and runs its finalizers) before the outer one. Scoped [`Layer`s](../part2/ch06-00-layers.md) (`Layer::scoped`, `Layer::acquire_release`) use the same machinery, so every resource a layer builds is released when the layer's scope closes.
