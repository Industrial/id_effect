# Workflow bridge (labs)

Durable state machines are **labs** functionality (ADR 0010 Amendment 1): the bridge moved out of `id_effect_fsm` into the unpublished `id_effect_workflow::fsm` module under `labs/`. It is not on crates.io and carries no stability promise.

The bridge persists `StateMachine` snapshots as JSON rows through a `StepJournal` (for example the SQLite-backed durable workflow log). After a restart it reloads the latest snapshot into a `StateMachine` with `set_state`, and cached steps skip re-execution — the same resume semantics as workflow steps.

To try it, work inside the labs workspace:

```bash
cargo run --manifest-path labs/Cargo.toml -p id_effect_workflow --example 030_durable_door_fsm
```

The published `id_effect_fsm` API it builds on — `StateMachine`, `TransitionTable`, `FsmError` — is stable, so a machine defined with `#[derive(Fsm)]` can be made durable later without changes to its definition.
