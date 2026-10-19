# Workflow bridge

A `StateMachine` lives in memory. To make one durable, drive it from a workflow of the published
`id_effect_workflow` crate ([Workflow and cluster](../part6/ch32-00-workflow-cluster.md)): the
events arrive as signals (`ctx.wait_signal`), each transition runs inside a journaled step
(`ctx.step`), and the side effects of entering a state are activities. After a restart the engine
replays the instance's history: the journaled events and states come back in order and nothing
runs twice, so the machine is rebuilt with `set_state` from the last journaled state instead of a
snapshot table.

<!-- ignore: id_effect_workflow is not a book_snippets dependency -->
```rust,ignore
/// Feeds the `door` signals into the machine until it is `Locked`. `Door` and `DoorEvent`
/// also derive `Schema`, so they can be journaled.
fn drive(
  ctx: WorkflowContext,
  mut machine: StateMachine<Door, DoorEvent>,
) -> Effect<Door, WorkflowError> {
  let next = ctx.clone();
  ctx.wait_signal::<DoorEvent>("door").flat_map(move |event| {
    let step = next.clone();
    let moved = match machine.step(event) {
      Ok(door) => succeed(door),
      Err(e) => fail(e.to_string()),
    };
    // Journaled: a replay returns the stored state, and a rejected event fails the same way.
    step.step("door.transition", moved).flat_map(move |door| {
      machine.set_state(door);
      if door == Door::Locked { succeed(door) } else { drive(next, machine) }
    })
  })
}
```

The unpublished bridge that persisted snapshots through a duroxide or SQLite `StepJournal`
(`labs/id_effect_workflow::fsm`) was removed in 0.11 together with the labs crate. The published
`id_effect_fsm` API it built on (`StateMachine`, `TransitionTable`, `FsmError`, `#[derive(Fsm)]`)
is unchanged, so a machine defined for it is driven by a workflow without changes to its
definition. A long-running behaviour with timeouts and postponed events is the
[`Statem` behaviour](./ch19-05-statem.md), supervised in memory; the durable timers of a workflow
(`ctx.sleep`) cover timeouts that must survive restarts.
