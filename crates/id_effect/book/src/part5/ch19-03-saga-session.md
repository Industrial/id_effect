# Sagas and session types

## Saga compensation

[`Saga`](../../id_effect_fsm/src/saga.rs) runs forward [`Effect`](../../id_effect/src/kernel/effect.rs) steps via `run_blocking`. On failure it invokes compensation factories in **reverse** order.

## Linear session types

`[`SessionSend<P>`](../../id_effect_fsm/src/`session.rs) and [``Session`Recv<P>`](../../id_effect_fsm/src/session.rs) are phantom markers encoding alternating send/receive phases. The included ping/pong protocol demonstrates type-directed hand-off without runtime protocol state machines.
