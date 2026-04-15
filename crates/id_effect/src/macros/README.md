# `macros` — declarative & procedural macro surface

This module is the **stable `effect::macros::*` path** for macros implemented in sibling crates:

- **`effect-macro`** — declarative macros: `pipe!`, `ctx!`, `req!`, `err!`, `service_key!`, `service_def!`, `layer_graph!`, `layer_node!`, …
- **`effect-proc-macro`** — procedural macros: `effect!`, `effect_tagged!`, `EffectData`, …

Rust cannot mix `macro_rules!` and `#[proc_macro]` in one crate, so the `effect` crate **re-exports** them at the root (`pub use id_effect_macro::…`, `pub use id_effect_proc_macro::…`) and exposes `macros::id_effect::effect` as a stable path for the procedural `effect!` macro.

## What lives here

| Item | Role |
|------|------|
| `macros::effect` | Re-export of `id_effect_proc_macro::effect` — do-notation for `Effect`. |
| Re-exports (see [`mod.rs`](mod.rs)) | Same symbols as `use id_effect::{pipe, ctx, …}` for module-qualified imports. |

## What it is used for

- **`effect!`** — primary syntax for monadic bind (`x ~ expr`), service extraction (`~ServiceTag`), and tail `Ok(value)` success.
- **`pipe!` / `ctx!`** — ergonomic wiring without deep nesting.
- **Service/layer graphs** — `layer_graph!`, `layer_node!`, `service_def!` for DI codegen patterns.

## Best practices

1. **Import from `effect` crate root** in application code unless you need `effect::macros::…` for disambiguation.
2. **Keep macro expansions reviewable** — avoid giant generated graphs without structure; split `layer_graph!` nodes.
3. **Follow** `.cursor/skills/effect.rs-fundamentals/SKILL.md` — `effect!` is the blessed style for new effectful code.
4. **Tests** for macro hygiene belong in `effect-macro` / `effect-proc-macro` crates; this folder is mostly documentation + re-export glue.

## See also

- [`kernel`](../kernel/README.md) — what `effect!` builds.
- Crates `crates/id_effect_macro`, `crates/id_effect_proc_macro` — implementations.
- [`SPEC.md`](../../SPEC.md) — naming and macro-related conventions.
