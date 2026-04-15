# `context` — Stratum 3: environment & compile-time wiring

**Tagged heterogeneous lists** for dependency injection: [`Tag<K>`](tag.rs), [`Tagged<K, V>`](tagged.rs), [`Cons` / `Nil`](hlist.rs), path types [`Here` / `SkipN` / `There`](path.rs), and [`Get` / `GetMut`](get.rs) for **compile-time** lookup. [`Context`](wrapper.rs) wraps the list for a convenient API.

## What lives here

| Module | Role |
|--------|------|
| `tag` | Phantom key types distinguishing services. |
| `tagged` | `Tagged<K, V>` — value `V` at key `K`. |
| `hlist` | `Cons` / `Nil` — type-level list of tagged cells. |
| `path` | `Here`, `Skip0`…`Skip4`, `There`, `ThereHere` — compile-time paths into the list. |
| `get` | `Get`, `GetMut` — trait-based projection. |
| `wrapper` | `Context<L>` — `new`, `prepend`, `get`, `get_path`, etc. |
| `match_` | `Matcher`, `HasTag` — type-level matching utilities. |
| `optics` | `EnvLens`, `focus` — optional lens-style focus (advanced). |

## What it is used for

- **Building `R` in `Effect<A, E, R>`** as a precise list of services (DB, clock, logger, …).
- **Ensuring** missing or mistyped dependencies are **compile errors**, not runtime map lookups.
- **Composing** layers that **prepend** cells; stack mirrors the type-level list order.

## Best practices

1. **Define one marker type per service** (`struct MyDbKey;`) and use `Tagged<MyDbKey, DbConn>`.
2. **Use `NeedsX` supertraits** in app crates (pattern in project skill) instead of raw `Get<…>` at every call site.
3. **Prefer `Get` / `prepend_cell`** over stringly-typed service maps.
4. **Keep `Context` values immutable** across most of an effect; use `GetMut` only where mutation is intentional and documented.

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 3.
- [`layer`](../layer/README.md) — constructing `Context` via layers.
- [`kernel`](../kernel/README.md) — `Effect` that consumes `R`.
