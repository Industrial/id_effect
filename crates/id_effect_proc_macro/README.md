# `id_effect_proc_macro`

Procedural macros for the [`id_effect`](https://github.com/Industrial/id_effect) workspace.
This is a `proc-macro = true` crate: most users depend on `id_effect` (or `id_effect_fsm` for
`#[derive(Fsm)]`), which re-export these macros. Declarative macros live in `id_effect_macro`.

## Macros

| Macro | Kind | Re-exported from | Generates |
|-------|------|------------------|-----------|
| `effect!` | function-like | `id_effect` | Do-notation for `Effect` (`~expr` binds) |
| `Schema` | derive | `id_effect` | `schema()` + `HasSchema` for named-field structs |
| `EffectData` | derive | `id_effect` | Structural `PartialEq` / `Eq` / `Hash` |
| `effect_tagged` | attribute | `id_effect` | `_tag` field + `HasTag` impl |
| `ProviderSpec` | derive | `id_effect` (as `ProviderSpecDerive`) | `ProviderSpec` impl from `#[provides(..)]` |
| `match_effect!` | function-like | `id_effect` | Exhaustive enum match helper |
| `Optics` | derive | `id_effect` | Field lenses and enum prisms for `id_effect_optics` |
| `Fsm` | derive | `id_effect_fsm` | `id_effect_fsm::Fsm` impl: initial state + transition table |
| `distributed_behavior` | attribute | — (use this crate) | `BEHAVIOR_NAME` + registration for remote spawn |

## Example

```rust
use id_effect::schema::HasSchema;
use id_effect::{Effect, Schema, effect, run_blocking, succeed};

#[derive(Clone, Debug, PartialEq, Schema)]
struct User {
  #[schema("user_name")]
  name: String,
  age: i64,
}

fn older(user: User) -> Effect<User, String, ()> {
  effect! {
    let age = ~succeed::<i64, String, ()>(user.age + 1);
    User { age, ..user }
  }
}

let wire = User::schema().encode(User { name: "Ada".into(), age: 36 });
let user = <User as HasSchema>::schema().decode_unknown(&wire).unwrap();
assert_eq!(run_blocking(older(user), ()).unwrap().age, 37);
```

## `#[derive(Fsm)]`

```rust,ignore
use id_effect_fsm::Fsm;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DoorEvent { Open, Close }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Fsm)]
#[fsm(event = DoorEvent, initial = Closed)]   // `initial` defaults to the first variant
enum Door {
  #[fsm(on(Open => Opened))]                   // bare events resolve to `DoorEvent::Open`
  Closed,
  #[fsm(on(Close => Closed))]
  Opened,
}

let mut door = Door::state_machine();
door.step(DoorEvent::Open).unwrap();
```

Unknown targets or `initial`, data-carrying variants, generic enums, and duplicate
`(state, event)` edges are compile errors. Use `#[fsm(crate = path)]` when re-exporting
`id_effect_fsm` under another name.

## Examples and tests

```bash
cargo run -p id_effect_proc_macro --example derive_schema
cargo nextest run -p id_effect_proc_macro
```
