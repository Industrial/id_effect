# `algebra` — Stratum 1: reusable algebraic structure

This directory encodes **functor → applicative → monad** (and related) patterns that recur across the crate. Rust has no higher-kinded types; APIs combine **traits with associated types** and **free functions** on concrete types (see each submodule).

## What lives here

| Module | Role |
|--------|------|
| `semigroup` | `Semigroup` — associative combine. |
| `monoid` | `Monoid` — semigroup + empty. |
| `functor` | `Functor` — `map` for a single type parameter. |
| `bifunctor` | `Bifunctor` — map over two parameters (e.g. success/error). |
| `contravariant` | `Contravariant` — contravariant maps (environments, sinks). |
| `applicative` | `Applicative` — `pure` / `ap`-style sequencing where laws apply. |
| `monad` | `Monad` — `flat_map` / bind-style composition. |
| `selective` | `Selective` — conditional / branching applicative structure. |
| `interface` | Shared interfaces / glue used by concrete instances. |

## What it is used for

- **Documenting laws** (associativity, identity, functor/monad laws) at the abstraction that matches Haskell-style names.
- **Sharing vocabulary** between `kernel::Effect` combinators and pure helpers on `Option`, `Result`, etc.
- **Extending** the system with new type constructors that still satisfy the same algebraic contracts.

## Best practices

1. **Prefer crate-root `Effect` combinators** for application code; use these traits when implementing **new** type constructors or proving laws in tests.
2. **Keep `where` clauses honest** — if a function needs `Monad`, say so; do not silently require a stronger constraint.
3. **Test laws** with `#[cfg(test)]` modules and `rstest` tables (see repo `TESTING.md`).
4. **Do not** use this layer for I/O — it stays pure; effects start at [`kernel`](../kernel/README.md).

## See also

- [`SPEC.md`](../../SPEC.md) §Stratum 1.
- [`foundation`](../foundation/README.md) — primitives this stratum builds on.
- [`kernel`](../kernel/README.md) — `Effect` uses functor/monad structure in practice.
