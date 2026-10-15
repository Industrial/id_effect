# Resources: `Pool`, `KeyedPool`, `Cache`

0.7 ports the v1 `resource` module to the new kernel (Effect.ts `Pool` / `KeyedPool` /
`Cache`). The constructors are plain functions now. They are no longer `make() -> Effect`.

```rust
// 0.6
let pool = run_blocking(Pool::make(4, connect), ())?;
let cache = run_async(Cache::make(128, Some(ttl), lookup), ()).await?;

// 0.7
let pool = Pool::new(4, connect).with_ttl(idle_ttl).with_release(close);
let keyed = KeyedPool::new(2, |host: Host| connect_to(host));      // 2 per key
let cache = Cache::new(128, Some(ttl), lookup);

Effect::scoped(move |scope| pool.get(&scope).flat_map(use_conn))  // checked in on close
cache.get(key)                                                     // Effect<V, E, R>
```

- `Pool::get` takes the scope as an argument (`pool.get(&scope)`). It no longer uses a `Scope`
  environment. The item is checked back in by a scope finalizer, however the scope closes,
  interrupt included. Waiting for capacity is interruptible and leaks no capacity. Acquiring a
  new item runs uninterruptibly, as in Effect.ts `acquireRelease`. A failing acquire gives its
  capacity back.
- `Pool<A, E, R>` / `KeyedPool<K, A, E, R>` / `Cache<K, V, E, R>`: the acquire and lookup
  effects may need capabilities `R`, and `get` then needs the same `R`. `E` defaults to
  `Never` and `R` to `HNil`.
- New: `with_release(hook)` disposes of an item that is invalidated, expires, or outlives
  `shutdown()`. `shutdown()` is an infallible `HNil` effect, so `scope.add_finalizer(pool.shutdown())`
  ties a pool's lifetime to a scope. After shutdown, checkouts are interrupted.
- `make_with_ttl(cap, ttl, f)` is now `new(cap, f).with_ttl(ttl)`. An idle item is released
  once it has been idle for `ttl`. The time is read from the executor clock, so it is virtual
  time on `TestExecutor`.
- **`KeyedPool` capacity is per key** (Effect.ts semantics). In 0.6 it was one global capacity
  shared by all keys. `KeyedPool::new_with(|key| n, acquire)` sets a capacity for each key.
- **`Cache` caches typed failures** for the TTL, as Effect.ts does with the lookup's `Exit`.
  0.6 cached only successes. Defects are passed on to the callers but are not cached. An
  interrupted lookup is not cached either, and the callers waiting on it retry. `Cache::get` fails with
  `E`, not `Cause<E>`. The full cause still propagates.
- `Cache::invalidate` no longer interrupts the callers waiting on a lookup in flight. They
  still get its result, but the result is not stored. Also new: `set`, `refresh`,
  `get_if_present`, `contains`, `invalidate_all` and `size`. `stats()` now reports
  `{ hits, misses, size }`, as in Effect.ts `CacheStats`. 0.6's `evictions` and `loads` are
  gone.
- TTL and capacity: `capacity == 0` still means unbounded. Least-recently-used eviction is
  unchanged.
