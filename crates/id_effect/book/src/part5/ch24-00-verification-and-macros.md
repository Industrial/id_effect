# Verification and Metaprogramming

Part V closes with tools that keep functional patterns honest: law checks, property tests, readable failure output, and derive macros.

## What This Chapter Covers

- **[`law_test!`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/law_test.rs)** (`id_effect_fp`) — monad law checks for concrete type constructors
- **[`failure::pretty`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/failure/pretty.rs)** — multi-line `Cause` / `Exit` rendering for failures
- **[`FreeAp`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/free_ap.rs)** (`id_effect_fp`) — free applicative over `Effect`
- **`id_effect_proc_macro` derives** — `#[derive(Optics)]` (lenses and prisms) and `#[derive(Schema)]` (`schema()`, `HasSchema`)

## Property tests with `Exit`

`Exit<A, E>` is `PartialEq` and `Debug`, so effects drop straight into `proptest` (or any property-testing crate): build the effect from the generated input, run it with `block_on` — or on a `TestExecutor` when it sleeps (Part IV [Testing](../part4/ch15-00-testing.md)) — and compare exits.

```rust
use id_effect::{Effect, Exit, succeed};

fn double(n: i64) -> Effect<i64, String> {
    succeed(n * 2)
}

// Inside `proptest! { #[test] fn doubles(n in -1000i64..1000) { … } }`:
let n = 21;
assert_eq!(double(n).block_on(), Exit::succeed(n * 2));
```

The 0.6 `testing::proptest` and `testing::snapshot` helpers (`success_value`, `prop_assert_exit_success`, `GoldenBuilder`, `assert_golden*`) and the `proptest` cargo feature were removed in 0.7 with the old kernel; plain `Exit` assertions replace them.

## Monad law checks

[`law_test!`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/law_test.rs) expands to a `#[test]` checking left identity, right identity and associativity. `f` and `g` must be `fn` items:

```rust
use id_effect_fp::algebra::monad::option;
use id_effect_fp::law_test;

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

[`pretty_cause`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/failure/pretty.rs) renders an indented cause tree (`Both` / `Then` branches nested); [`pretty_exit`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect/src/failure/pretty.rs) labels success vs failure for logs and test output. `pretty_cause_inline` gives a one-line form. All are re-exported from `id_effect::failure`.

```rust
use id_effect::failure::pretty_exit;
use id_effect::{Effect, fail};

let eff: Effect<u8, &str> = fail("boom");
let exit = eff.block_on();
println!("{}", pretty_exit(&exit));
```

## Free applicative

[`FreeAp`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/free_ap.rs) collects effectful work as data, then [`interpret`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_fp/src/algebra/free_ap.rs) runs it as a concrete `Effect`:

```rust
use id_effect::{Exit, HNil, succeed};
use id_effect_fp::FreeAp;

let free: FreeAp<i32, (), HNil> = FreeAp::ap2(
    |a: i32, b: i32| a + b,
    FreeAp::lift(succeed(2)),
    FreeAp::lift(succeed(3)),
);
assert_eq!(free.interpret().block_on(), Exit::succeed(5));
```

## Derive macros

`id_effect_proc_macro` ships derives for optics and schema codegen (both re-exported by `id_effect`):

| Derive | Generates |
|--------|-----------|
| `Optics` | `id_effect_optics` field lenses and enum prisms |
| `Schema` | inherent `schema()` + `HasSchema` over the JSON-shaped `Unknown` wire |

```rust
use id_effect::{Optics, Schema};

#[derive(Optics)]
struct Point { x: i32, y: i32 }

#[derive(Clone, Schema)]
struct User { #[schema("user_name")] name: String }
```

`#[derive(Optics)]` generates field lenses and enum prisms backed by `id_effect_optics`. `#[derive(Schema)]` generates `schema()` and a `HasSchema` impl; `#[schema("wire")]` renames a field on the wire. For a JSON text parser, pass the type to the labs `id_effect_parse::SchemaBridge::parser_for::<User>()` (see [Parser Combinators](./ch20-00-parser-combinators.md)).

The 0.6 deprecated alias of `Schema` was removed in 0.7; use `#[derive(Schema)]`.

`#[derive(Fsm)]` lives in `id_effect_fsm` (re-exported from `id_effect_proc_macro`): annotate a state enum with `#[fsm(event = …, initial = …)]` and `#[fsm(on(Event => Target, …))]` per variant to generate its transition table. Unknown targets and duplicate edges are compile errors. See the [state machines chapter](./ch19-00-state-machines.md).
