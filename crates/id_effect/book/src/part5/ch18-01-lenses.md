# Lenses

A [`Lens<S, A>`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/lens.rs) is a **total** optic: every `S` has exactly one focused `A`.

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

Compose nested lenses with [`Lens::compose`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/lens.rs).

To skip the hand-written getter/setter pairs, `#[derive(Optics)]` (from `id_effect_proc_macro`) generates a lens per named field. See [Verification and metaprogramming](./ch24-00-verification-and-macros.md).
