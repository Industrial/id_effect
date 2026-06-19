# Capability Keys and `Env` — Compile-Time Service Lookup

Chapter 4 showed how `R` encodes dependencies. For small programs a single key in `caps!(K)` is enough. As the graph grows you need **named capability keys** so the compiler can distinguish services — even when they share the same Rust type.

This chapter covers:

- Why positional/tuple `R` breaks down
- [`#[capability]`](../../src/capability/key.rs) — declaring key types
- [`Env`](../../src/capability/env.rs) — the order-independent runtime container
- [`Needs<K>`](../../src/capability/needs.rs) and `~Key` — accessing services inside effects

By the end you'll know how capability lookup works and why insertion order in `Env` never matters.
