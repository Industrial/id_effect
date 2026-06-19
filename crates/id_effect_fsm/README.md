# `id_effect_fsm`

Typed **finite-state machines**, **sagas**, and **linear session types** for [`id_effect`](https://github.com/Industrial/id_effect) programs. Part of the FP Kitchen Sink roadmap (Plan 03 / Part V ch19).

## Modules

| Module | Role |
|--------|------|
| `machine` | Pure `TransitionTable` + `StateMachine::step` |
| `interpreter` | Run transition hooks via `run_blocking` |
| `visualize` | Export Mermaid `stateDiagram-v2` charts |
| `matcher` | Bridge events/states to `HasTag` / `Matcher` |
| `saga` | Forward steps with LIFO compensation |
| `session` | Linear `SessionSend` / `SessionRecv` protocol markers |
| `workflow` | Persist FSM snapshots through `id_effect_workflow` |

## Quick example

```rust
use id_effect_fsm::{StateMachine, TransitionTable};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum S { Idle, Done }
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum E { Go }

let table = TransitionTable::new().on(S::Idle, E::Go, S::Done);
let mut m = StateMachine::new(S::Idle, table);
m.step(E::Go).expect("transition");
assert_eq!(m.state(), S::Done);
```

## Book

See mdBook **Part V — ch19 State Machines** (`crates/id_effect/book/src/part5/`).

## Tests

```bash
cargo test -p id_effect_fsm
```
