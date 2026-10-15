# Error Handling Inside effect!

A failing `~` ends the whole `effect!` block with that failure. You can also handle errors within
the block.

## The Default: Short-Circuit

```rust,ignore
effect!({
  let a = ~step_a();  // if this fails, the block fails
  let b = ~step_b(a); // if this fails, the block fails
  b
})
```

This matches `?` on a `Result`, and `?` works too: on a `Result` it fails the block with the
error (converted with `From`):

```rust
# use id_effect::{Cause, Effect, Exit, effect};
fn parse(raw: &'static str) -> Effect<i32, &'static str> {
  effect!({ raw.parse::<i32>().map_err(|_| "parse_failed")? })
}

assert_eq!(parse("42").block_on(), Exit::succeed(42));
assert_eq!(parse("x").block_on(), Exit::Failure(Cause::Fail("parse_failed")));
```

## Catching Errors Mid-block

To recover and continue, use `.catch` before the `~`:

```rust,ignore
effect!({
  let v = ~parse_i32("x").catch(|_| succeed(0));
  v + 42
})
```

`.catch(|e| ..)` turns a typed failure into another effect (a fallback value, or a different
failure). Defects and interrupts are not caught by `catch`.

## Converting Errors

A bound effect's error is widened into the block's error through `From`, so with

```rust,ignore
#[derive(Debug)]
enum AppError {
  Db(DbError),
  Network(HttpError),
}
impl From<DbError> for AppError { fn from(e: DbError) -> Self { AppError::Db(e) } }
impl From<HttpError> for AppError { fn from(e: HttpError) -> Self { AppError::Network(e) } }
```

both steps bind without any conversion:

```rust,ignore
fn load(id: u64) -> Effect<Data, AppError, caps!(Database, HttpClient)> {
  effect!({
    let user = ~fetch_user(id);             // DbError -> AppError
    let data = ~fetch_external(user.id);    // HttpError -> AppError
    process(user, data)
  })
}
```

Without a `From` impl, convert explicitly with `.map_err(..)` before the `~`.

## Inspecting the Outcome

`.exit()` turns the outcome into a value, so the block sees success and failure alike:

```rust,ignore
effect!({
  let outcome = ~risky_operation().exit();
  match outcome {
    Exit::Success(v) => format!("ok: {v}"),
    Exit::Failure(cause) => format!("failed: {cause:?}"),
  }
})
```

## Re-raising Errors

Inside `.catch`, decide whether to recover or fail again:

```rust,ignore
effect!({
  ~db_operation().catch(|error| {
    if error.is_transient() { fallback_db_operation() } else { fail(error) }
  })
})
```

## Accumulating Multiple Errors

Short-circuiting stops at the first error. When you need every failure (form validation), validate
outside the short-circuiting chain and collect the results; Chapter 8 covers error accumulation.

## The Rule of Thumb

| Want | Do |
|------|----|
| Stop at the first failure | plain `~effect` or `?` on a `Result` |
| Provide a fallback | `~effect.catch(\|e\| fallback)` |
| Unify error types | `impl From<Small> for Big`, or `.map_err(..)` |
| Look at success and failure | `~effect.exit()` |
| Collect all failures | accumulate outside the chain (Chapter 8) |
