# Chaining Effects — flat_map and the Bind

`map` handles a pure transformation `A → B`. Often the next step is itself an effect: you don't
want `Effect<Effect<B, E, R>, E, R>`, you want `Effect<B, E, R>`. That's `flat_map`.

## The Problem with map for Effects

```rust
fn get_user(id: u64) -> Effect<User, DbError, caps!(Database)> { /* ... */ }
fn get_posts(user_id: u64) -> Effect<Vec<Post>, DbError, caps!(Database)> { /* ... */ }

// Effect<Effect<Vec<Post>, DbError, _>, DbError, _>: a nested effect, not what we want
let wrong = get_user(1).map(|user| get_posts(user.id));
```

## flat_map: Chain Without Nesting

`flat_map` takes a function `A → Effect<B, E, R>` and flattens the result:

```rust
let combined: Effect<Vec<Post>, DbError, caps!(Database)> =
  get_user(1).flat_map(|user| get_posts(user.id));
```

When run, it fetches the user, then uses the result to fetch the posts. Both sides of a
`flat_map` have the same `E` and `R`.

```rust
use id_effect::{Effect, Exit, succeed};

let program: Effect<i32> = succeed(20).flat_map(|n: i32| succeed(n + 22));
assert_eq!(program.block_on(), Exit::succeed(42));
```

## Chaining Multiple Steps

Deep chains get noisy:

```rust
let program = get_user(1).flat_map(|user| {
  get_posts(user.id).flat_map(move |posts| render_page(user, posts))
});
```

This is where the `effect!` macro comes in.

## The effect! Macro as Syntactic Sugar

`effect!` turns `flat_map` chains into sequential code with the `~` operator:

```rust
use id_effect::{Effect, caps, effect};

fn page() -> Effect<Page, AppError, caps!(Database)> {
  effect!({
    let user = ~get_user(1);
    let posts = ~get_posts(user.id);
    render_page(user, posts)
  })
}
```

`~expr` is the bind: "run this effect and give me its success value". The whole block is one
effect. `render_page` (no `~`) is an ordinary Rust expression that runs as part of the body.
`~` also widens each step's error into the block's error (`AppError: From<DbError>` here), so the
`.map_err` calls disappear.

## Error Short-Circuiting

Like `?` on a `Result`, if any `~` step fails, the whole block stops with that error:

```rust
effect!({
  let user = ~get_user(999); // if this fails, the block ends here
  let posts = ~get_posts(user.id);
  render_page(user, posts)
})
```

The steps are sequential. To run independent steps concurrently, bind a tuple:
`let (a, b) = ~(fetch_a(), fetch_b());` joins them on child fibers.

## map vs flat_map — When to Use Each

| Situation | Use |
|-----------|-----|
| Transformation returns a plain value | `.map(f)` |
| Transformation returns an effect | `.flat_map(f)` or `~` in `effect!` |
| More than one sequential step | `effect!({ .. })` |

## The Full Picture

```rust
// Equivalent:

// 1. Explicit flat_map
get_user(1).flat_map(|user| get_posts(user.id))

// 2. effect! with ~
effect!({
  let user = ~get_user(1);
  ~get_posts(user.id)
})
```

`effect!` is the idiomatic choice for anything with more than one step. Chapter 3 covers it in
full.
