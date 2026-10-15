# Parallel combinators: `zip_par`, `all`, `for_each_par`

0.7 adds structured parallel combinators on fibers (Effect.ts `Effect.all` / `Effect.forEach`
with `concurrency`). They replace hand-rolled `fork` + `fiber_all`.

```rust
// 0.6: fork each effect, then join the handles
// 0.7
zip_par(load_user(id), load_orders(id))                    // Effect<(User, Vec<Order>), E, R>
zip_with_par(a, b, |a, b| a + b)
all(effects, Concurrency::Bounded(4))                      // Vec<A>, input order
for_each_par(ids, |id| fetch(id), Concurrency::Unbounded)  // Vec<A>, input order
all(effects, Concurrency::Sequential)                      // a left fold, nothing forked
```

- The first failure interrupts the in-flight siblings and starts no further ones; the result
  is returned only after every started child has finished (its finalizers have run).
- The cause is that first failure. A sibling that also fails genuinely (`Fail` or `Die`, e.g.
  inside an uninterruptible region or in its finalizer) is added as `Cause::Both(first, other)`.
  Interrupts of siblings stopped by the first failure are not added, so a lone failure is
  exactly that `Cause::Fail(e)`.
- `Concurrency::Bounded(n)` is a local window. `Concurrency::Inherit` awaits admission from the
  ambient `Fabric` (`Fabric::provide`); with no fabric it behaves as `Unbounded`
  (see `c-fabric-scheduler.md`).
