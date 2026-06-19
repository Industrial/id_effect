# ADR 0005 — Capability subset projection (`CapProjectAt`)

## Status

Accepted

## Context

ADR 0004 introduced `CapWiden` as **prefix-only** tuple projection. Real programs need any single key from a wider `caps!(…)` list. An interim `CapWidenSecond` trait was added for two-key lists; it does not scale and requires manual `zoom_env`.

## Decision

### Single-key projection by index

Per-index projection methods on `CapList` (arities 1–8): `project_at_0` … `project_at_7`. Runtime clones shared `Env`; type-level drops non-selected keys.

### Automatic bind via `CapBind`

`cap_into_bind` dispatches to macro-generated `CapBind` impls for each `(wide_arity, index)` pair. Remove prefix-only bind paths and `CapWidenSecond`.

### Non-goals

- Arbitrary multi-key subset projection
- Normalizing `caps!(…)` key order
- Duplicate key types in one `caps!(…)` with automatic bind

## Consequences

- Book/examples drop `zoom_env` + `CapWidenSecond` for common composition
- Pre-publish 3.0.0 ships with automatic single-key subtyping
