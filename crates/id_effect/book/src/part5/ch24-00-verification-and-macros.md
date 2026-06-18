# Verification and Metaprogramming

Part V closes with tools that keep functional patterns honest: law checks, property tests, golden snapshots, and derive stubs that will grow into full codegen.

## What This Chapter Covers

- **[`testing::proptest`](../../src/testing/proptest.rs)** — helpers for `run_test` + `Exit` in property tests
- **[`law_test!`](../../src/algebra/law_test.rs)** — monad law checks for concrete type constructors
- **[`failure::pretty`](../../src/failure/pretty.rs)** — multi-line `Cause` / `Exit` rendering for failures
- **[`testing::snapshot`](../../src/testing/snapshot.rs)** — golden snapshot builders (`GoldenBuilder`, `assert_golden`)
- **[`FreeAp`](../../src/algebra/free_ap.rs)** — free applicative over `Effect`
- **`id_effect_proc_macro` stubs** — `#[derive(Optics)]`, `#[derive(Fsm)]`, `#[derive(SchemaParser)]`

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

Use [`law_test!`](../../src/algebra/law_test.rs) with **function items** (not closure literals) for `f` and `g`:

```rust
use id_effect::law_test;
use id_effect::algebra::monad::option;

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

[`pretty_cause`](../../src/failure/pretty.rs) renders indented trees; [`pretty_exit`](../../src/failure/pretty.rs) labels success vs failure branches for logs and test output.

## Golden snapshots

[`GoldenBuilder`](../../src/testing/snapshot.rs) freezes expected strings; [`assert_golden_effect`](../../src/testing/snapshot.rs) runs an effect and asserts the snapshot contract.

```rust
use id_effect::{GoldenBuilder, snapshot_effect_map_flat_map, assert_golden_effect};

assert_golden_effect(snapshot_effect_map_flat_map(), ());
GoldenBuilder::new("my_case", "expected").assert_observed("observed");
```

## Free applicative

[`FreeAp`](../../src/algebra/free_ap.rs) collects effectful work as data, then [`interpret`](../../src/algebra/free_ap.rs) runs it as a concrete `Effect`:

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

## Derive stubs (proc macros)

`id_effect_proc_macro` ships minimal compiles-today derives reserved for future crates:

| Derive | Reserved for |
|--------|----------------|
| `Optics` | `id_effect_optics` lens/prism codegen |
| `Fsm` | `id_effect_fsm` transition tables |
| `SchemaParser` | `id_effect_parse` schema-driven parsers |

```rust
use id_effect_proc_macro::{Optics, Fsm, SchemaParser};

#[derive(Optics)]
struct Point { x: i32, y: i32 }

#[derive(Fsm)]
enum Light { Red, Green }

#[derive(SchemaParser)]
struct User { name: String }
```

Each derive emits a hidden stub constant so callers can opt in before full codegen lands.
