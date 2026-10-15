# The ~ Operator Explained

Inside `effect!`, the prefix `~` has three forms:

| Form | Meaning |
|------|---------|
| `~expr` (an effect) | run the sub-effect on this fiber, evaluate to its success value, stop on failure |
| `~Cap` (a capability type) | read capability `Cap` from the environment `R` (an `&Arc<Cap>`) |
| `~(a, b, ..)` | run up to 8 effects in parallel on child fibers and evaluate to the tuple of values |

## Binding an effect

```rust,ignore
effect!({
  let user = ~fetch_user(42);
  user.name
})
```

When you don't need the value, use `~` as a statement:

```rust,ignore
effect!({
  ~log_event("processing started");
  let result = ~do_work();
  ~log_event("processing done");
  result
})
```

A bound effect may need only **part** of the block's `R` (it is narrowed into the block's
environment), and its error is **widened** into the block's error: `Never` converts to anything,
any other `E2` needs `E: From<E2>`.

## Reading a capability

```rust,ignore
fn program() -> Effect<i32, Never, caps!(Counter)> {
  effect!({ ~Counter.0 + 1 })
}

assert_eq!(program().provide(Counter(41)).block_on(), Exit::succeed(42));
```

`~Counter` reads the `Counter` in `R` (an `&Arc<Counter>`); field accesses and method calls after
it are plain Rust on that reference (`~Logger.info(msg)`). `~X` is a capability read only when `X`
is an upper-case identifier (not followed by `::`, `(`, `{` or `!`); anything else after `~` is
bound as an effect. So when a capability's method returns an effect, read the capability first and
bind the call: `let repo = ~UserRepo.clone(); let user = ~repo.get_user(id);`. Reading a
capability that is not in `R` is a compile error naming it.

## Joining in parallel

```rust,ignore
effect!({
  let (user, prefs) = ~(fetch_user(id), fetch_prefs(id));
  render(user, prefs)
})
```

The branches run concurrently; the first failure fails the join and interrupts the siblings.

## Method chains on effects

`~` applies to the whole expression after it, so combinators go before the bind:

```rust,ignore
effect!({
  let user = ~fetch_user(id).map_err(AppError::Database);
  let posts = ~fetch_posts(user.id).timeout(Duration::from_secs(2)); // Option<Vec<Post>>
  (user, posts)
})
```

## ~ in conditionals and loops

```rust,ignore
effect!({
  let value = if condition { ~compute_a() } else { ~compute_b() };
  for id in user_ids {
    ~process_user(id); // sequential: one at a time
  }
  value
})
```

Loops are sequential. For concurrent work over a collection use `for_each_par` or `all`
([Chapter 9](../part3/ch09-00-concurrency.md)).

## What ~ Cannot Do

`~` only works inside an `effect!` (or `effect_local!`) body:

```rust,ignore
// Does not compile: ~ is not valid here
let x = ~fetch_user(42);

// Inside effect!
let x: Effect<User, DbError> = effect!({ ~fetch_user(42) });
```

Inside a `from_async(|| async move { .. })` block you are in plain async Rust: use `.await` there
and `~` outside it.

## Binding a generic helper

`~` narrows from any sub-environment, so a helper that is generic in its `R` leaves nothing to fix
that `R`. Bind it with `.same_env()`, which ties it to the caller's environment:

```rust,ignore
fn audit<R, I>(msg: String) -> Effect<(), AppError, R>
where
  R: HList + Selector<Logger, I>,
{
  effect!({ ~Logger.info(msg) })
}

fn signup() -> Effect<(), AppError, caps!(Logger, Db)> {
  effect!({ ~audit("signup".to_string()).same_env() })
}
```

A helper with a concrete `caps!(..)` needs nothing special.
