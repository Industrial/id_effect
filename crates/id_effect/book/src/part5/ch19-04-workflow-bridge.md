# Workflow bridge

[`register_fsm`](../../id_effect_fsm/src/workflow.rs) and [`step_durable`](../../id_effect_fsm/src/workflow.rs) persist [`FsmSnapshot`](../../id_effect_fsm/src/workflow.rs) JSON rows through [`DurableWorkflowLog`](../part2/ch07-12-durable-workflow.md).

After a restart, [`restore_state`](../../id_effect_fsm/src/workflow.rs) reloads the latest snapshot into a `StateMachine`. Cached steps skip re-execution — the same resume semantics as workflow `run_step_typed`.
