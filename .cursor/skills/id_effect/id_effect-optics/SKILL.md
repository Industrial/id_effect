---
name: id_effect-optics
description: >-
  Teaches id_effect_optics: Lens, Prism, Optional, Traversal, transducers,
  schema field paths on Unknown, JSON patch (add/replace/remove/move/copy/test), TrieZipper navigation/rebuild. Use when
  focusing/updating nested immutable data or schema documents — Part V ch18.
---

# id_effect Optics

**Part V ch18**. Default: **compose small optics** instead of ad-hoc path strings in domain code.

**Prerequisite**: `id_effect-fundamentals`, `id_effect-schema` for `Unknown` paths.

## Decision tree

```
Nested product field (struct)?
  → Lens + field() or compose

Sum-type / Option variant?
  → Prism or Optional

Many elements (Vec / im::Vector)?
  → Traversal (vector_each, im_vector_each)

Boundary document (Unknown)?
  → schema_bridge get_at_path / set_at_path, json_patch apply_patch
```

## Lens

```rust
use id_effect_optics::{Lens, field};

let city = address_lens.compose(field(|a: &Address| &a.city, |mut a, c| { a.city = c; a }));
let updated = city.modify(person, |c| format!("{c}, UK"));
```

## Prism / Optional

```rust
use id_effect_optics::{Prism, Optional, some_prism};

let circle = Prism::new(/* preview */, /* review */);
let nick = Optional::new(nickname_lens).set_some(profile, "ada".into());
```

## Schema paths

```rust
use id_effect_optics::{get_at_path, create_at_path, apply_patch, PatchOp};

let name = get_at_path(&doc, "user.name")?;
let patched = apply_patch(doc, &PatchOp::Replace { path: "count".into(), value: Unknown::I64(2) })?;
```

## Not this → but that

| Not this | But that |
|----------|----------|
| Manual clone-and-rebuild helpers | `Lens::modify` / `Traversal::over` |
| String paths in domain logic | Optics in core; paths only at `Unknown` boundary |
| String paths in domain logic | Optics in core; paths only at `Unknown` boundary |

## Derive

```rust
use id_effect_proc_macro::Optics;

#[derive(Optics)]
struct Point { x: i32, y: i32 }

let lens = Point::x_lens();
```

## Crate & example

```bash
cargo test -p id_effect_optics
cargo run -p id_effect_optics --example 010_lens
```

Book: `crates/id_effect/book/src/part5/ch18-00-optics.md`

## Next

- Parse codecs (ch20): `id_effect-schema` + optics derive (Plan 04)
- Streaming transducers (ch22)
