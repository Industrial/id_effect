# Lenses

A [`Lens<S, A>`](../../../../id_effect_optics/src/lens.rs) is a **total** optic: every `S` has exactly one focused `A`.

```rust
use id_effect_optics::{Lens, field};

#[derive(Clone)]
struct Person { name: String }

let name = field(
    |p: &Person| &p.name,
    |mut p, name| { p.name = name; p },
);

let updated = name.modify(Person { name: "ada".into() }, |n| n.to_uppercase());
```

Compose nested lenses with [`Lens::compose`](../../../../id_effect_optics/src/lens.rs).

> **Stub:** derive-generated field lenses land in Plan 04 (FP DX).
