# State Machines (`id_effect_fsm`)

The **`id_effect_fsm`** crate adds typed finite-state machines and sagas on top of `id_effect`, plus a `gen_statem`-style process behaviour for running a machine under supervision.

## Why FSMs in an effect library?

Business logic often has **explicit states** (order lifecycle, connection handshake, approval gates). A pure transition table keeps routing **declarative**; `Effect` values attach **side effects** to the edges, and the whole transition is itself an `Effect` you compose and interpret like any other.

## Crate layout

- `TransitionTable` / `StateMachine` — immutable edge map, current state + pure `step`
- `Fsm` + `#[derive(Fsm)]` — declare the initial state and edges on an enum
- `Interpreter` — Effect-valued transition actions; `step` / `run` return an `Effect`
- `FsmProcess` / `FsmHandle` — host a machine in an `id_effect_process` process (events are mailbox messages)
- `Saga` / `SagaStep` — forward steps with LIFO compensation, as one `Effect`
- `to_mermaid` — documentation diagrams

## Durable state machines

Persisting machine snapshots across restarts is labs functionality: the unpublished `id_effect_workflow::fsm` module (see [Workflow bridge](./ch19-04-workflow-bridge.md)). For multi-service orchestration, prefer an external engine (Temporal, Step Functions).
