# State Machines (`id_effect_fsm`)

The **`id_effect_fsm`** crate adds typed finite-state machines, sagas, and linear session types on top of `id_effect`. It is the Phase **FP FSM** slice from the [FP Kitchen Sink roadmap](../../../../docs/fp-patterns/ROADMAP.md).

## Why FSMs in an effect library?

Business logic often has **explicit states** (order lifecycle, connection handshake, approval gates). A pure transition table keeps routing **declarative**; `Effect` programs attach **side effects** at the edges via `run_blocking`.

## Crate layout

- [`TransitionTable`](../../id_effect_fsm/src/machine.rs) — immutable edge map
- [`StateMachine`](../../id_effect_fsm/src/machine.rs) — mutable current state + `step`
- [`Interpreter`](../../id_effect_fsm/src/interpreter.rs) — effectful stepping
- [`to_mermaid`](../../id_effect_fsm/src/visualize.rs) — documentation diagrams
- [`Saga`](../../id_effect_fsm/src/saga.rs) — compensate on failure
- [`SessionSend` / `SessionRecv`](../../id_effect_fsm/src/session.rs) — linear protocols
- [`step_durable`](../../id_effect_fsm/src/workflow.rs) — SQLite snapshots via `id_effect_workflow`

## When to use workflow bridge

For **single-process restart resume**, pair FSM stepping with [`id_effect_workflow`](../part2/ch07-12-durable-workflow.md). For multi-service orchestration, prefer an external engine (Temporal, Step Functions).
