# `id_effect_lints`

[Dylint] lints for idiomatic [id_effect] code: `Effect<A, E, R>` signatures, `effect!`, services,
`from_async`, fibers, time, and raw SQL (`raw_sql_non_literal`, ADR 0013 R5). `cargo dylint` runs
them the way `cargo clippy` runs Clippy's.

## Install and run

```bash
cargo install cargo-dylint dylint-link
```

```toml
# Your workspace's Cargo.toml
[workspace.metadata.dylint]
libraries = [
  { git = "https://github.com/Industrial/id_effect", tag = "v0.11.0", pattern = "lints/id_effect_lints" },
]
```

```bash
cargo dylint --all          # warn
cargo dylint --all --fix    # apply the MachineApplicable suggestions
```

Dylint builds this library with the nightly pinned in `rust-toolchain.toml` (it links against
`rustc`'s own crates); your project keeps its toolchain. Every lint warns by default
(`tests_must_use_run_test` is opt-in). Raise one with `DYLINT_RUSTFLAGS="-D <lint>"` or
`#![cfg_attr(dylint_lib = "id_effect_lints", deny(<lint>))]`.

## Lints

Declared in `src/lib.rs`, categories A–M: function signatures, `effect!` discipline,
async/`from_async`, the `block_on` edge and `TestExecutor` tests, services (typed `R`, `caps!`,
`Layer`), errors, concurrency, time, tests, logging, schema boundaries, `Effect` combinators
(`effect_map_identity`, `effect_flat_map_succeed`, `effect_flat_map_then`: auto-fixed) and raw
SQL (`raw_sql_non_literal`: the `&str` methods of `SqlClient` / `SqlTransaction` / `SqlExecutor`
get a literal, a `const`, or `Sql::as_str()`).

The ast-grep rules in `rules/` (`sgconfig.yml`) cover the same ground without nightly:
`ast-grep scan --config lints/id_effect_lints/sgconfig.yml`.

`examples/demo` is a small crate that trips both: `cd examples/demo && cargo dylint --all`, or
`ast-grep scan -c ../../sgconfig.yml .` from the same directory. CI checks each finding.

## Developing

```bash
cd lints/id_effect_lints
cargo test        # UI tests: ui/<lint>.rs + .stderr (.fixed for --fix), ui_test/ with --test
```

The library is not a member of the main workspace (its own `[workspace]`, toolchain and
`dylint-link` linker in `.cargo/config.toml`). When the expected output changes, compiletest
prints where it saved the actual `.stderr`; review it and copy it over. Bump the nightly together
with `dylint_linting`.

## License

MIT, see the workspace root in [id_effect].

[Dylint]: https://github.com/trailofbits/dylint
[id_effect]: https://github.com/Industrial/id_effect
