# Verification and Metaprogramming

Part V closes with tools that keep functional patterns honest: law checks, property tests, golden snapshots, and derive macros.

## What This Chapter Covers

- **[`testing::proptest`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/testing/proptest.rs)** — helpers for `run_test` + `Exit` in property tests
- **[`law_test!`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/law_test.rs)** — monad law checks for concrete type constructors
- **[`failure::pretty`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/failure/pretty.rs)** — multi-line `Cause` / `Exit` rendering for failures
- **[`testing::snapshot`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/testing/snapshot.rs)** — golden snapshot builders (`GoldenBuilder`, `assert_golden`)
- **[`FreeAp`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/free_ap.rs)** — free applicative over `Effect`
- **`id_effect_proc_macro` derives** — `#[derive(Optics)]` (lenses and prisms) and `#[derive(Schema)]` (`schema()`, `HasSchema`)

## Property tests with `Exit`

Enable the optional `proptest` feature when you want strategy helpers:

```toml
[dev-dependencies]
id_effect = { version = "3", features = ["proptest"] }
proptest = "1"
```

Core helpers work without the feature:

```rust
use id_effect::{run_effect, exit_success_value, Exit, succeed};

let exit = run_effect(succeed(42), ());
assert_eq!(exit_success_value(exit), Some(42));
```

With `proptest`, use `success_value` and `prop_assert_exit_success` inside `proptest!` blocks (see Part IV [Property Testing](../part4/ch15-04-property-testing.md)).

## Monad law checks

Use [`law_test!`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/law_test.rs) with **function items** (not closure literals) for `f` and `g`:

```rust
use id_effect::law_test;
use id_effect_fp::algebra::monad::option;

fn inc(x: i32) -> Option<i32> { Some(x + 1) }
fn double(x: i32) -> Option<i32> { Some(x * 2) }

law_test! {
  monad option_i32 {
    pure = option::pure,
    flat_map = option::flat_map,
    fa = Some(3),
    a = 7,
    f = inc,
    g = double,
  }
}
```

## Pretty failures

[`pretty_cause`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/failure/pretty.rs) renders indented trees; [`pretty_exit`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/failure/pretty.rs) labels success vs failure branches for logs and test output.

## Golden snapshots

[`GoldenBuilder`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/testing/snapshot.rs) freezes expected strings; [`assert_golden_effect`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/testing/snapshot.rs) runs an effect and asserts the snapshot contract.

```rust
use id_effect::{GoldenBuilder, snapshot_effect_map_flat_map, assert_golden_effect};

assert_golden_effect(snapshot_effect_map_flat_map(), ());
GoldenBuilder::new("my_case", "expected").assert_observed("observed");
```

## Free applicative

[`FreeAp`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/free_ap.rs) collects effectful work as data, then [`interpret`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/free_ap.rs) runs it as a concrete `Effect`:

```rust
use id_effect::{FreeAp, pure, run_test, Exit};

let free = FreeAp::ap2(
  |a: i32, b: i32| a + b,
  FreeAp::lift(pure(2)),
  FreeAp::lift(pure(3)),
);
let exit = run_test(free.interpret(), ());
assert_eq!(exit, Exit::succeed(5));
```

## Derive macros

`id_effect_proc_macro` ships derives for optics and schema codegen:

| Derive | Generates |
|--------|-----------|
| `Optics` | `id_effect_optics` field lenses and enum prisms |
| `Schema` | inherent `schema()` + `HasSchema` over the JSON-shaped `Unknown` wire |
| `SchemaParser` | deprecated alias of `Schema` (identical output) |

```rust
use id_effect_proc_macro::{Optics, Schema};

#[derive(Optics)]
struct Point { x: i32, y: i32 }

#[derive(Clone, Schema)]
struct User { #[schema("user_name")] name: String }
```

`#[derive(Optics)]` generates field lenses and enum prisms backed by `id_effect_optics`. `#[derive(Schema)]` generates `schema()` and a `HasSchema` impl; `#[schema("wire")]` renames a field on the wire. For a JSON text parser, pass the type to `id_effect_parse::SchemaBridge::parser_for::<User>()`.

`#[derive(SchemaParser)]` is kept as a deprecated alias for one release. It no longer generates a `parser()` method (that tied a published derive to the unpublished `id_effect_parse` crate). Derive macros cannot carry `#[deprecated]`, so there is no compiler warning — switch to `#[derive(Schema)]`.

`#[derive(Fsm)]` lives in `id_effect_fsm` (re-exported from `id_effect_proc_macro`): annotate a state enum with `#[fsm(event = …, initial = …)]` and `#[fsm(on(Event => Target, …))]` per variant to generate its transition table. Unknown targets and duplicate edges are compile errors. See the [state machines chapter](./ch19-00-state-machines.md).
