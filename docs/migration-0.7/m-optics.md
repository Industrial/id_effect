# `id_effect_optics`: no change for users

`id_effect_optics` does not use the Effect API. It has no `Effect`, `Exit`, `Cause`, `Fiber`,
`Layer` or `Runtime` in its source, and it never calls `run_blocking` or `block_on`. The
kernel switch-over therefore leaves its public API unchanged:

```rust
// 0.6 and 0.7 — identical
use id_effect::schema::Unknown;
use id_effect_optics::{Lens, PatchOp, Transducer, apply_patch, field, get_at_path, map};
```

The crate depends on `id_effect` for two things:

- `id_effect::schema::Unknown`, used by `schema_bridge` and `json_patch`. The schema module is
  kernel-neutral and stays at the crate root.
- the re-export `id_effect_optics::transducer::{Reducer, Transducer, filter, map, take}`. This is
  the same type that `Stream::transduce_items` uses. Today it comes from
  `id_effect::streaming::transducer`. When the v1 `streaming` module is retired, the re-export
  will point at the `next` streaming module instead. The names and signatures stay the same, so
  user code does not change.
