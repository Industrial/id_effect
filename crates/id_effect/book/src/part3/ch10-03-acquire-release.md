# acquire_use_release — The Bracket Pattern

`acquire_use_release(acquire, use_, release)` pairs acquisition and release into a single effect (Effect.ts `acquireUseRelease`, the classic *bracket*):

- `acquire` runs **uninterruptibly**, so a resource is never half-acquired;
- `use_` gets the resource and runs with the **caller's interruptibility**;
- `release` runs **uninterruptibly, exactly once** when `acquire` succeeded, and receives the `ExitKind` of `use_` (`Success`, `Failure`, `Die` or `Interrupt`). A failed `acquire` releases nothing.

From `examples/090_acquire_release.rs`:

```rust
use std::sync::{Arc, Mutex};
use id_effect::{Effect, Exit, ExitKind, acquire_use_release, succeed, sync};

let log = Arc::new(Mutex::new(Vec::new()));
let log2 = Arc::clone(&log);
let eff: Effect<u8, &'static str> =
    acquire_use_release(succeed(9_u8), succeed, move |n, _kind: ExitKind| {
        sync(move || log2.lock().unwrap().push(n))
    });
assert_eq!(eff.block_on(), Exit::succeed(9));
assert_eq!(*log.lock().unwrap(), vec![9]);
```

The resource type must be `Clone` (one copy goes to `use_`, one to `release`), so a connection is typically an `Arc` or a cheap handle. `release` returns an `Effect<(), Never, R>`: it cannot fail, but it may use the same capabilities as the rest of the effect.

## Why Bracket?

`acquire_use_release` makes the acquisition-release pair inseparable. You can't call `open_connection()` without also registering its cleanup, and you can't forget the cleanup on the error path:

```rust,ignore
// Without bracket: cleanup is manual and easy to skip on failure or interrupt.
let conn = ~open_connection();
let result = ~use_connection(&conn);
~close_connection(conn);

// With bracket: cleanup is mandatory and automatic.
let result = ~acquire_use_release(open_connection(), use_connection, |conn, _| close_connection(conn));
```

A common convention is a helper that owns the bracket:

```rust,ignore
fn with_connection<A: Send + 'static>(
    f: impl FnOnce(Conn) -> Effect<A, DbError, caps!(DbConfig)> + Send + 'static,
) -> Effect<A, DbError, caps!(DbConfig)> {
    acquire_use_release(open_connection(), f, |conn, kind| close_connection(conn, kind))
}
```

## Bracket vs. Scope

- Use **`acquire_use_release`** when the resource's lifetime is exactly one `use_` effect.
- Use **`Effect::scoped`** + `scope.add_finalizer(..)` when several resources share one lifetime, or when the cleanup is registered in the middle of a longer body ([Scopes and Finalizers](./ch10-02-scopes-finalizers.md)).
- Use **`Layer::acquire_release`** / **`Layer::scoped`** when the resource is an application capability built once and released at shutdown.

## Bracket vs. Drop

`acquire_use_release` is not a replacement for `impl Drop` — it's a complement:

- `Drop`: synchronous cleanup for memory and OS resources;
- `acquire_use_release`: effectful cleanup (async I/O, capabilities, logging) that must run on every exit path, including interruption.

A `TcpStream` closing its file descriptor in `Drop` is fine. Gracefully closing a database session that needs a round trip belongs in a bracket.
