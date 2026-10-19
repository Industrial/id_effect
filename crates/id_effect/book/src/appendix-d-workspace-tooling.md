# Workspace tooling (macros and lints)

This appendix covers **authoring** and **static analysis** pieces that most application readers skip—but contributors and advanced users need to know where they live.

## `id_effect_macro` and `id_effect_proc_macro`

The macro surface is split across two crates, both re-exported at the `id_effect` root (and in
`id_effect::macros`):

- **`id_effect_proc_macro`** — procedural macros: `effect!` / `effect_local!` (do-notation over
  a typed `R`), `caps!`, and the derives (`Schema`, `EffectData`, `ProviderSpec` as
  `ProviderSpecDerive`, `Optics`, `Fsm`, …).
- **`id_effect_macro`** — declarative macros: `pipe!` and `err!`. (The 0.6 capability-DI macros
  `provide!`, `providers!`, `require!` and `mock_capability!` were removed in 0.7.)

Expansions name root paths (`::id_effect::HCons`, `::id_effect::__private::…`), so depend on
`id_effect` rather than on these crates directly. The 0.7 trybuild suite
(`crates/id_effect/tests/ui`) pins the macro diagnostics.

When debugging “why doesn’t my `effect!` compile?”, use **`cargo expand`** on a small repro and inspect the generated code. Application code should keep following [The effect! Macro](./part1/ch03-00-effect-macro.md); these crates are implementation details unless you extend the macro system.

## `id_effect_lints` (Dylint)

`id_effect_lints` is a [Dylint](https://github.com/trailofbits/dylint) library: id_effect's own
lints, run by `cargo dylint` next to `cargo clippy`. It lives at
[`lints/id_effect_lints`](https://github.com/Industrial/id_effect/tree/main/lints/id_effect_lints),
outside the main workspace, because a Dylint library links against the compiler's own crates and
so builds with the nightly pinned in its `rust-toolchain.toml`. Your project keeps its own
toolchain: Dylint installs the pinned nightly and builds the library with it.

### Install

```bash
cargo install cargo-dylint dylint-link
```

Then name the library in your workspace's `Cargo.toml`, pinned to the id_effect release you use:

```toml
[workspace.metadata.dylint]
libraries = [
  { git = "https://github.com/Industrial/id_effect", tag = "v0.11.0", pattern = "lints/id_effect_lints" },
]
```

A single-package project puts the same table under `[package.metadata.dylint]`. Without the
metadata, name the library on the command line:
`cargo dylint --git https://github.com/Industrial/id_effect --tag v0.11.0 --pattern lints/id_effect_lints --all`.

### Use

```bash
cargo dylint --all                    # every library named in the metadata
cargo dylint --all -- --all-targets   # arguments after `--` go to `cargo check`
cargo dylint --all --fix              # apply the MachineApplicable suggestions
```

Every lint warns by default (`tests_must_use_run_test` is opt-in). Raise one to an error with
`DYLINT_RUSTFLAGS="-D raw_sql_non_literal" cargo dylint --all`, or in code, where a plain
`#[deny(...)]` would be an unknown lint for ordinary `rustc`:

```rust,ignore
#![cfg_attr(dylint_lib = "id_effect_lints", deny(raw_sql_non_literal))]

#[cfg_attr(dylint_lib = "id_effect_lints", allow(no_result_return_in_pub_fn))]
pub fn parse(s: &str) -> Result<u32, std::num::ParseIntError> { s.parse() }
```

### The lints

The lint names and categories are listed in `src/lib.rs`, each with a UI test in `ui/`
(`ui_test/` for the ones that look at `#[test]` functions). Categories A–K cover function
signatures (`Effect` instead of `async fn` / `Result`, `where` clauses, `caps!` over 0.6
environments), `effect!` discipline, `.await` outside `from_async`, the `block_on` edge,
`tokio::spawn` / `thread::spawn`, services in statics or thread-locals, `.provide` in library code,
stringly errors, `unwrap` / `panic!` / `println!` in Effect code, discarded `.fork()`,
`Arc<Mutex<_>>` / `Rc<RefCell<_>>`, `thread::sleep` and the wall clock, and raw `serde_json`
parsing. Two later categories:

- **L, combinators** (MachineApplicable, so `cargo dylint --fix` rewrites them):
  `effect_map_identity` (`.map(|x| x)` is removed), `effect_flat_map_succeed`
  (`.flat_map(|x| succeed(f(x)))` becomes `.map(|x| f(x))`) and `effect_flat_map_then`
  (`.flat_map(move |_| next)` becomes `.then(next)`).
- **M, raw SQL**: `raw_sql_non_literal` warns when the SQL passed to the `&str` methods of
  `SqlClient`, `SqlTransaction` or `SqlExecutor` (`query`, `execute`, `query_stream`) is not a
  literal, a `const`/`static`, a local bound once to one, a `&'static str` parameter, or
  `Sql::as_str()`. The effect-level `query`/`execute` already reject a `String` at compile time
  (they take `impl Into<Sql>`); the traits are the driver boundary and take any `&str`, so this
  lint is
  [ADR 0013](https://github.com/Industrial/id_effect/blob/main/docs/adrs/0013-secure-by-default.md)
  R5's second enforcement point. SQL that really is built at run time goes through
  `query_raw` / `execute_raw` with a `RawSql` and the `RawSqlCapability`. A literal wrapped in a
  `String` (`&"…".to_string()`) is fixed automatically.

The ast-grep rules in `lints/id_effect_lints/rules/` cover the same ground without a nightly
toolchain: `ast-grep scan --config lints/id_effect_lints/sgconfig.yml`. Their severities are
ast-grep's (`error`, `warning`, `hint`), so a scan that finds an `error` rule exits non-zero;
`--warning` reports everything without failing.

### A runnable example

[`lints/id_effect_lints/examples/demo`](https://github.com/Industrial/id_effect/tree/main/lints/id_effect_lints/examples/demo)
is a dependency-free crate that trips a handful of lints: a `pub fn` returning `Result`, an
`.unwrap()` in an `Effect`-returning function, `SystemTime::now()`, an `Rc<RefCell<_>>`, a
`std::thread::spawn` and SQL built with `format!`. Its `Cargo.toml` names the library by path, so
both tools run from its directory:

```bash
cd lints/id_effect_lints/examples/demo
cargo dylint --all                      # no_result_return_in_pub_fn, …, raw_sql_non_literal
ast-grep scan -c ../../sgconfig.yml .   # the same findings, except raw_sql_non_literal
```

`raw_sql_non_literal` is Dylint-only: it needs the type of the SQL argument, which a syntax
match cannot see.

### In this repository

`.github/workflows/lint.yml` runs `cargo dylint --path lints/id_effect_lints --all -- --workspace`
on every push and pull request, then fails unless each of the demo crate's lints fires. Its
`ast-grep` job installs a pinned, checksum-verified ast-grep, checks that the rules load and fire
on the demo crate, and reports the workspace's findings per rule in the job summary. Work on the
library itself from its directory:
`cd lints/id_effect_lints && cargo test` runs the UI tests (rustup picks the pinned nightly; the
`.fixed` files check what `--fix` produces).

## Platform scorecard

Each release is scored against ten reference frameworks on 82 capabilities. The rules are in
[`docs/scorecard/METHOD.md`](https://github.com/Industrial/id_effect/blob/main/docs/scorecard/METHOD.md),
and the rendered matrix, with a version selector and the planned path to 1.0, is
[`docs/scorecard/index.html`](https://github.com/Industrial/id_effect/blob/main/docs/scorecard/index.html).
`scripts/scorecard.sh` computes every number from `docs/scorecard/scorecard.json`, writes the
per-release report (for example `docs/scorecard/REPORT-0.7.0.md`) and runs the release gates
`--diff` and `--compare-targets`.
