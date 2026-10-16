# Why Do-Notation Exists

Consider three steps that each depend on the previous result:

```rust
# use id_effect::{Effect, succeed};
# type Err = String;
fn step_a() -> Effect<i32, Err> { succeed(1) }
fn step_b(n: i32) -> Effect<i32, Err> { succeed(n * 2) }
fn step_c(n: i32) -> Effect<String, Err> { succeed(n.to_string()) }
```

Written with raw `flat_map`:

```rust
# use id_effect::{Effect, Exit, succeed};
# type Err = String;
# fn step_a() -> Effect<i32, Err> { succeed(1) }
# fn step_b(n: i32) -> Effect<i32, Err> { succeed(n * 2) }
# fn step_c(n: i32) -> Effect<String, Err> { succeed(n.to_string()) }
let program = step_a().flat_map(|a| step_b(a).flat_map(|b| step_c(b)));
# assert_eq!(program.block_on(), Exit::succeed("2".to_string()));
```

Two steps: readable. Five steps: a pyramid. Ten steps: callback hell.

Haskell solved this with *do-notation*, Scala with for-comprehensions. Rust has no built-in
do-notation for custom types, so id_effect provides it as a macro.

## Do-Notation as a Concept

```text
do
  a ← step_a
  b ← step_b(a)
  c ← step_c(b)
  return c
```

Each `←` means "run this effect and bind its result to this name". If any step fails, the whole
computation short-circuits. id_effect spells `←` as a prefix `~`:

```rust
# use id_effect::{Effect, Exit, effect, succeed};
# type Err = String;
# fn step_a() -> Effect<i32, Err> { succeed(1) }
# fn step_b(n: i32) -> Effect<i32, Err> { succeed(n * 2) }
# fn step_c(n: i32) -> Effect<String, Err> { succeed(n.to_string()) }
# let program: Effect<String, Err> =
effect!({
  let a = ~step_a();
  let b = ~step_b(a);
  let c = ~step_c(b);
  c
})
# ;
# assert_eq!(program.block_on(), Exit::succeed("2".to_string()));
```

## How It Works

The body of `effect!` is compiled into an async block that runs on the effect's fiber. Each
`~expr` runs the sub-effect `expr` on that same fiber and evaluates to its success value; a failure
ends the block with that failure. The result behaves like the hand-written chain:

```rust
# use id_effect::{Effect, succeed};
# type Err = String;
# fn step_a() -> Effect<i32, Err> { succeed(1) }
# fn step_b(n: i32) -> Effect<i32, Err> { succeed(n * 2) }
# fn step_c(n: i32) -> Effect<String, Err> { succeed(n.to_string()) }
# let _: Effect<String, Err> =
step_a().flat_map(|a| step_b(a).flat_map(step_c))
# ;
```

Because the body is ordinary async Rust, `if`, `match`, loops and `let` bindings work as usual, and
the value of the last expression is the success value. The body must be `Send` (use
`effect_local!` for `!Send` state, which builds a `LocalEffect`).

## One Body, One Block

Use **one `effect!` per function** and branch inside it:

```rust
# use id_effect::{Effect, effect, succeed};
# fn a() -> Effect<i32, String> { succeed(1) }
# fn b() -> Effect<i32, String> { succeed(2) }
# let flag = true;
// BAD: two macro bodies for one computation
# let _: Effect<i32, String> =
if flag {
  effect!({ ~a() })
} else {
  effect!({ ~b() })
}
# ;

// GOOD: one block, branching inside
# let _: Effect<i32, String> =
effect!({
  if flag { ~a() } else { ~b() }
})
# ;
```

## Pure Expressions

Not every line has to be an effect:

```rust
# use id_effect::{Effect, effect, succeed};
# struct User { id: u64, name: String }
# struct Post;
# fn fetch_user(id: u64) -> Effect<User, String> { succeed(User { id, name: "ada".into() }) }
# fn fetch_posts(_: u64) -> Effect<Vec<Post>, String> { succeed(vec![]) }
# let id = 1;
# let _: Effect<(String, Vec<Post>), String> =
effect!({
  let user = ~fetch_user(id);
  let name = user.name.to_uppercase(); // pure: no ~
  let posts = ~fetch_posts(user.id);
  (name, posts)
})
# ;
```

Use `~` only on expressions of type `Effect<_, _, _>` (and on capability types, Chapter 4).
