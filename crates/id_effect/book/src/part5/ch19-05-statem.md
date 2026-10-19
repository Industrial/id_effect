# `Statem`: gen_statem parity

`FsmProcess` hosts a table-driven machine whose states are `Copy` keys. Protocols with timers, buffered requests and per-state data need more: Erlang's `gen_statem`. `id_effect_fsm::Statem` is that behaviour, added next to the `Fsm` trait (which is unchanged).

A `Statem` is a callback module. The implementing type is never instantiated; it names the machine's types and callbacks:

| Associated type | Role |
|---|---|
| `State` | The state. May carry data: `Clone + PartialEq + Debug`, no `Copy`. |
| `Data` | The machine data, which travels with the machine through every callback (needs no `Clone`). |
| `Event` | The payload of calls, casts, infos, timeout messages and inserted events. |
| `Reply` | What a call answers. |
| `Args`, `Env`, `Error` | Init arguments, the callbacks' effect environment and their error, as for any `Process`. |

`handle_event(event, state, data, ctx)` returns `Effect<Transition<Self>, Error, Env>`:

- `Transition::Next(state, data, actions)` moves to `state`. This is a **state change** only when the new state differs (`!=`) from the current one.
- `Transition::Keep(data, actions)` stays in the current state.
- `Transition::Stop(reason)` exits the process with `reason`.

The event says how it arrived: `StatemEvent::{Call(e, reply_to), Cast(e), Info(e), StateTimeout(e), EventTimeout(e), GenericTimeout(name, e), Internal(e)}`.

## Actions

Actions run in order after the callback that returned them:

| Action | Semantics |
|---|---|
| `Postpone` | Keep the current event aside. Postponed events are re-delivered after the next state change, in arrival order. |
| `StateTimeout(d, e)` | Deliver `StateTimeout(e)` after `d`. There is one per machine, and arming it replaces the previous one. A state change cancels it, and a `StateTimeout` returned together with the change arms the new state's timeout. |
| `CancelStateTimeout` | Cancel the pending state timeout. |
| `EventTimeout(d, e)` | Deliver `EventTimeout(e)` after `d` unless any event arrives first. Inserted and re-delivered events count. |
| `GenericTimeout(name, d, e)` / `CancelGenericTimeout(name)` | Named timers. Arming a name replaces its timer. They are never cancelled automatically. |
| `Reply(reply_to, value)` | Answer a call. `ReplyTo` is cloneable, so a postponed call can be answered from a later event. |
| `NextEvent(e)` | Handle `Internal(e)` before any postponed or mailbox event. |

The optional `on_enter(from, state, data, ctx)` callback runs for the initial state (with `from = None`) and after every state change, before the next event. It can arm or cancel timeouts and reply. As in Erlang, returning `Postpone` or `NextEvent` from `on_enter` or `init` crashes the process.

Timers use `ProcessCtx::send_after` on the node's executor clock, so they run on virtual time under a `TestExecutor` and die with the process. Each timer carries a generation id. When a timeout is replaced or cancelled after its message has already reached the mailbox, the message is dropped, so a cancelled timeout is never delivered.

## Running it

`StatemProcess<M>` implements `id_effect_process::Process` (`Args = M::Args`, `Env = M::Env`). Start it with `StatemProcess::start(&node, args, env)` or `start_linked(&ctx, args, env)`, or supervise it with a `ChildSpec`. A restart runs `init` again. `StatemHandle` provides `call(event, timeout)` (the reply comes from an `Action::Reply`; if the machine exits first the call fails with `CallError::Down`), `cast`, `send` and `state`.

```rust,ignore
impl Statem for CodeLock {
    type State = Door;           // enum Door { Locked { failed: u32 }, Open }
    type Data = Keypad;          // the code and the buttons pressed so far
    type Event = Input;          // Button(u8) | Lock | Forget | Status
    // Args, Env, Reply, Error ...

    fn handle_event(event: &StatemEvent<Self>, door: &Door, mut keypad: Keypad, _ctx: ProcessCtx)
        -> Effect<Transition<Self>, String, ()>
    {
        succeed(match (event, door) {
            (StatemEvent::Cast(Input::Button(_)), Door::Open) =>
                Transition::Keep(keypad, vec![Action::Postpone]),
            (StatemEvent::StateTimeout(Input::Lock), Door::Open) =>
                Transition::Next(Door::Locked { failed: 0 }, keypad, vec![]),
            // ...
        })
    }

    fn on_enter(_from: Option<&Door>, door: &Door, keypad: Keypad, _ctx: ProcessCtx)
        -> Effect<Entered<Self>, String, ()>
    {
        let actions = match door {
            Door::Open => vec![Action::StateTimeout(OPEN_FOR, Input::Lock)],
            Door::Locked { .. } => vec![],
        };
        succeed((keypad, actions))
    }
}
```

Runnable example: `cargo run -p id_effect_fsm --example statem_door_lock`. The integration tests in `crates/id_effect_fsm/tests/statem.rs` pin down each rule above on virtual time: `statem_state_carries_data`, `postponed_event_redelivered_after_state_change`, `state_timeout_cancelled_on_transition`, `event_timeout_cancelled_by_any_event`, `enter_action_runs_on_every_state_change` and `statem_under_supervisor_restarts`.

## Coverage

Together with `FsmProcess`, this gives the scorecard row `fault.fsm` 8 of 8 checks at level *documented*. Check #3 (state data) is covered by `State` without `Copy` plus a separate `Data`. Check #4 (state and event timeouts) is covered by `StateTimeout` and `EventTimeout`. Check #5 (postpone) is covered by `Postpone`. State enter strengthens check #6.

Not covered yet: Erlang's `repeat_state` (re-running enter without a state change), `Exit`/`Down` system messages as machine events (a `StatemProcess` ignores them), and timeouts with absolute deadlines.
