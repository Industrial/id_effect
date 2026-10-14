# Prisms and Optionals

A [`Prism<S, A>`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/prism.rs) focuses a **sum-type** variant:

```rust
use id_effect_optics::Prism;

enum Shape { Circle(f64), Rect { w: f64, h: f64 } }

let circle = Prism::new(
    |s: &Shape| match s { Shape::Circle(r) => Some(*r), _ => None },
    Shape::Circle,
);
```

[`Optional`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_optics/src/optional.rs) wraps a `Lens<S, Option<T>>` for fallible inner access (`set_some`, `set_none`, `modify`).

`#[derive(Optics)]` generates a prism per enum variant. Composing prisms with schema tagged unions is not provided by the published crates.
