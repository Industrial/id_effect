# Traversable

**Traverse** runs an effectful function over a structure while preserving its shape. `traverse_vec` binds sequentially and stops at the first failure; `sequence_vec` turns a `Vec<Effect<A, E, R>>` into an `Effect<Vec<A>, E, R>`; `traverse_option` does the same for `Option`.

```rust
use id_effect::{Effect, Exit, succeed};
use id_effect_fp::algebra::traversable::traverse_vec;

let eff: Effect<Vec<i32>, ()> = traverse_vec(vec![1, 2], |n| succeed(n * 2));
assert_eq!(eff.block_on(), Exit::succeed(vec![2, 4]));
```

For parallel traversal use the core combinators `for_each_par` / `all` (Part III).
