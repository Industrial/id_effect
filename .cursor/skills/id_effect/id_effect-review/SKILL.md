---
name: id_effect-review
description: >-
  Reviews id_effect Rust code for idiomaticity, capability DI violations, removed API
  usage, effect! misuse, and test gaps. Use when reviewing pull requests, examining
  id_effect changes, or when the user asks for an id_effect code review.
---

# id_effect Review

Synthesis gate across all id_effect skills. Read changed files, then apply this checklist.

## Review workflow

1. Identify **layer** — kernel, domain, provider, binary edge.
2. Check **`Effect<_, _, caps!(…)>`** on public APIs — no bare service types as `R`.
3. Verify **run site** — `run_with` / `run_async` only at edge, not deep in domain.
4. Run mental **`cargo test --workspace`** — error paths and mocks via DI?
5. Classify findings (below).

## Severity

| Level | Meaning |
|-------|---------|
| **Critical** | Must fix — wrong DI, removed APIs, leaks, incorrect R types |
| **Suggestion** | Should fix — idioms, test gaps, macro misuse |
| **Nice** | Optional — naming, docs, example alignment with book |

## DI violations (Critical)

- [ ] Uses removed APIs: `Layer`, `Stack`, `ctx!`, `req!`, `define_capability!`, `.provide()`
- [ ] Positional `(Db, Logger)` as `R` instead of `caps!(DatabaseKey, LoggerKey)`
- [ ] Service trait methods with `R != ()` (requirement leakage)
- [ ] Constructs platform clients inside domain instead of `caps!(…)`
- [ ] `require!(env, Key)` old arity

## effect! (Suggestion / Critical)

- [ ] Postfix `~` syntax (removed)
- [ ] `~` outside `effect!` block
- [ ] Parallel logic inside `effect!` instead of `fiber_all` / `map_par_n`
- [ ] Nested `effect!` where a named function would clarify

## Errors & runtime (Critical)

- [ ] `panic!` / `.unwrap()` for expected business failures
- [ ] `run_blocking` in tests where `run_test` + `Exit` assertions needed
- [ ] Unjoined fibers (would fail under `run_test`)

## Streams & parallelism (Suggestion)

- [ ] Deprecated `*_par` without migration note
- [ ] Rayon for effectful IO (should be `map_par_n`)
- [ ] Missing `*_serial` when closure is `FnMut` / non-`Send`

## Book & docs alignment (Suggestion)

- [ ] Examples use `~Key`, `caps!(…)`, `run_with([provide!(…)], …)`
- [ ] Key suffix `Key` on capability types
- [ ] Says **id_effect capability DI** — not "v2 DI" or Layer terminology

## Verify commands

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo test -p id_effect --test ui_compile_fail
cd crates/id_effect/book && mdbook build
```

See [reference/checklist.md](reference/checklist.md) for extended checks.
