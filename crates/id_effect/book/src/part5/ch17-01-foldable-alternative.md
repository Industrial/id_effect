# Foldable and Alternative

**Foldable** summarizes collections; **Alternative** expresses choice (`empty` / `alt`).

```rust
use id_effect::algebra::foldable::option::fold_right;
use id_effect::algebra::alternative::option::{alt, empty};

let sum = fold_right(Some(3), 0, |a, b| a + b);
let chosen = alt(empty(), Some(42));
```
