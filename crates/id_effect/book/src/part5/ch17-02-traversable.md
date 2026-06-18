# Traversable

**Traverse** runs effectful functions over structures while preserving shape.

```rust
use id_effect::algebra::traversable::traverse_vec;
use id_effect::Effect;
use id_effect::runtime::run_blocking;

let eff = traverse_vec(vec![1, 2], |n| Effect::succeed(n * 2));
assert_eq!(run_blocking(eff, ()), Ok(vec![2, 4]));
```
