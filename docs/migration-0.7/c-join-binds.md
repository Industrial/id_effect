# Parallel binds: `~(a, b, ..)` in `effect!`

0.6's `effect!` ran independent `let x = ~a; let y = ~b;` statements in parallel on its own
(`join_binds2..4`, opt-out with `#[serial]`). In 0.7 binds are sequential, as in Effect.ts's
`Effect.gen`; parallelism is explicit, with a parenthesised list after `~`:

```rust
// 0.6: implicit, up to 4 independent binds
let user = ~load_user(id);
let orders = ~load_orders(id);

// 0.7: explicit, 2 to 8 effects, a tuple in input order
let (user, orders) = ~(load_user(id), load_orders(id));
```

- The branches run as child fibers (`all` over them, `Concurrency::Unbounded`), each over a
  part of the body's environment (or `.same_env()`), each error widened into the body's error
  like any `~` error.
- The first failure interrupts the siblings still running, the join waits for them (their
  finalizers run), and that first failure is the `~`'s outcome: its typed error goes to the
  body's `?`, a defect or interrupt becomes the body's cause. Genuine sibling failures, which
  `zip_par` adds as `Cause::Both(first, other)`, are not surfaced; bind
  `~zip_par(a, b).exit::<Never>()` to inspect the whole cause.
- `~(expr)` with no top-level comma is still a plain bind. `effect_local!` rejects
  `~(a, b, ..)` (its branches would have to be `Send` to fork).
- Several `ctx.bind(..)` futures awaited at once on the same fiber (a hand-written
  `futures::join!`) now each see an interrupt of the fiber; before, they held it off from each
  other. They still run interleaved on one fiber; `~(a, b)` is the parallel form.
