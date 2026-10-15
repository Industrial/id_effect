# FiberRef — Fiber-Local State

`FiberRef<T>` is the effect equivalent of thread-local storage, except that it follows the *fiber* across executor threads. Each fiber has its own slot; a forked child starts with a copy of its parent's value.

## Creating a FiberRef

```rust,ignore
use id_effect::FiberRef;

// A fiber-local trace id, "none" in every fiber until it is set.
let trace_id: FiberRef<String> = FiberRef::new("none".to_string());
```

`FiberRef::new(init)` is a plain constructor (`T: Clone + Send + Sync + 'static`). Cloning a `FiberRef` clones the *handle*: both clones name the same slot. Keep one in a `static` `LazyLock`, in a capability, or pass it around.

## Reading and Writing

`get`, `set` and `update` are effects (they need the running fiber):

```rust
use id_effect::{Effect, Exit, FiberRef};

let r = FiberRef::new(0_u32);
let r2 = r.clone();
let program: Effect<u32, ()> = r.set(42).flat_map(move |()| r2.get());
assert_eq!(program.block_on(), Exit::succeed(42));

// `locally` scopes a value to one effect, then restores the previous one.
let scoped: Effect<u32, ()> = r.locally(7, r.get());
assert_eq!(scoped.block_on(), Exit::succeed(7));
```

`locally(value, eff)` restores the previous value however `eff` ends (success, failure, defect or interrupt).

## Inheritance Across Forks

A child forked with `fork`, `fork_all` (and so `all`, `zip_par`, …) or `Scope::fork_in` starts with a **copy of its parent's values at fork time**. After that, each side's `set` is invisible to the other:

```rust,ignore
let program = trace_id.set("req-123".into())
    .then(trace_id.get().fork())          // the child sees "req-123"
    .flat_map(|h| h.join());
```

`fork_daemon` inherits the same copy (Effect.ts parity). Only a fiber started from outside any fiber (`Runtime::fork`, a run edge such as `block_on`) starts from the initial values.

## Common Use Cases

| Use Case | Pattern |
|----------|---------|
| Request tracing / correlation ids | `FiberRef<String>` set per request, read by loggers |
| Per-request user context | `FiberRef<Option<UserId>>` with `locally` |
| Metric tags | the metrics layer's fiber tags (`MetricRegistry::fiber_tags`) |
| Debug context | `FiberRef<Vec<String>>` call path |

Runtime integrations (tracing bridges, metrics) can read or write a fiber's value from outside an effect with `get_in(&fiber_context)` / `set_in`.
