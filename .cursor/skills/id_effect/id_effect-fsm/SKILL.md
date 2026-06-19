---
name: id_effect-fsm
description: >-
  Write and review id_effect_fsm — StateMachine, TransitionTable, Interpreter with run_blocking,
  to_mermaid, HasTag matcher bridge, Saga compensation, SessionSend/SessionRecv, workflow bridge.
  Use when editing id_effect_fsm or Part V ch19 book content.
---

# id_effect-fsm

**Crate:** `crates/id_effect_fsm` · **Book:** Part V ch19

## Core API

| Type | Use |
|------|-----|
| `TransitionTable::on(from, event, to)` | Build pure edges |
| `StateMachine::step(event)` | Pure transition |
| `Interpreter::on_transition(from, event, \|\| Effect…)` | Effect factory per edge |
| `to_mermaid` / `to_mermaid_display` | Mermaid export |
| `TaggedEvent` + `event_matcher` | `HasTag` / `Matcher` bridge |
| `Saga` + `SagaStep::with_compensate` | LIFO rollback |
| `SessionSend` / `SessionRecv` | Linear session markers |
| `register_fsm` / `step_durable` / `restore_state` | `id_effect_workflow` bridge |

## Rules

- **`Effect` is not `Clone`** — register `Fn() -> Effect<…>` factories, clone `Arc` captures inside the factory.
- **Pure vs effectful** — keep `StateMachine::step` pure; run effects in `Interpreter` via `run_blocking`.
- **Durable states** — `S: Serialize + Deserialize` for workflow snapshots.

## Verify

```bash
cargo test -p id_effect_fsm
cargo clippy -p id_effect_fsm -- -D warnings
```

## See also

- `id_effect-fundamentals` — `Effect`, `effect!`, `run_blocking`
- `id_effect-integration` — `id_effect_workflow` durable log
