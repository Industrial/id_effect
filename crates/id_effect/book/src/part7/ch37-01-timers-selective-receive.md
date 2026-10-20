# Timers, selective receive and hibernate

Three BEAM tools for processes that wait: timers (`erlang:send_after`, gen_server's `timeout`),
selective receive (`receive ... after`), and `hibernate`. All of them run on the node's executor
clock, so a node on a `TestExecutor` drives them with virtual time (`exec.advance(d)`).

## Timers: `send_after` and `cancel_timer`

`ProcessCtx::send_after::<Self>(msg, d)` delivers `msg` to the process's own `handle_info` (as
`Info::User(msg)`) after `d` and returns a `TimerRef`. Callers outside the process use
`Addr::send_after(msg, d)`.

```rust,ignore
fn handle_info(mut self, msg: Info<Msg>, ctx: ProcessCtx) -> Effect<Next<Self>, Never, ()> {
  if let Info::User(Msg::Tick) = msg {
    self.ticks += 1;
    self.next = Some(ctx.send_after::<Self>(Msg::Tick, Duration::from_millis(10)));
  }
  succeed(Next::Continue(self))
}

// later, in a handler:
let stopped = self.next.take().is_some_and(|t| ctx.cancel_timer(t));
```

- **Cancel is exact.** `ctx.cancel_timer(timer)` (or `timer.cancel()`) returns `true` when the
  timer was pending: its message will never be delivered. `false` means it already fired or was
  cancelled before. Firing and cancelling race on one atomic state, so exactly one wins. A
  cancelled timer's task ends at once, not at the deadline.
- **Timers die with their process.** When the process exits, for any reason, its pending timers
  are cancelled; `Addr::send_after` to a dead process returns an inert timer
  (`is_pending() == false`).
- **A timer message is a user message.** It counts against the mailbox bound when it fires
  (decision D17): a full mailbox drops it, exactly like `Addr::send_now`. Exit signals and
  monitor downs still bypass the bound.
- `send_after::<P>` names the behavior because `ProcessCtx` is untyped; inside a callback it is
  `Self`. With another behavior the timer is inert.

## Idle timeout: `Info::Timeout`

`ctx.set_idle_timeout(d)` makes the loop deliver `Info::Timeout` to `handle_info` when it has
waited `d` for the next message. It fires **once per idle period**: after a timeout the loop waits
without a deadline until a message arrives, and that message starts a new period.
`ctx.clear_idle_timeout()` removes it. The timeout never enters the mailbox, so a full mailbox
cannot hold it back, and no message is lost to the deadline: a message that arrives as the
deadline passes stays queued.

## Selective receive

`ctx.receive::<Self, _>(matcher, timeout)` takes the first message in the mailbox that
`matcher` accepts and leaves every skipped message queued, in arrival order, for the normal loop.
It fails with `ReceiveError::Timeout` once `timeout` passes, `ReceiveError::ScanLimit` once a
mailbox capacity's worth of messages was skipped, and `ReceiveError::Closed` when the process is
being killed. A matched `ProcMsg::Call` must be answered through its reply.

```rust,ignore
let reply = ctx
  .receive::<Self, ()>(|m| matches!(m, ProcMsg::Info(Info::User(Msg::Ack(id))) if *id == want), within)
  .exit::<Never>();
```

## Hibernate

A callback that returns `Next::Hibernate(state)` keeps the state but sheds memory while idle:

1. the loop runs the optional `Process::hibernate(self, ctx)` hook (default: keep the state), the
   place to drop caches and shrink collections; a failure crashes the process like a handler;
2. it releases the mailbox buffer (`Mailbox::shrink_to_fit`: a burst leaves the buffer at its
   peak size, and draining it does not give that memory back);
3. it waits for the next message as with `Continue`.

Rust has no per-process heap to compact or stack to discard, so this is the honest analogue of
BEAM's `hibernate`: what the process itself holds is released, not a garbage-collected heap.

## Breaking changes

`Next`, `Info` and `ExitReason` are `#[non_exhaustive]` (new variants: `Next::Hibernate`,
`Info::Timeout`, `ExitReason::NameConflict`). A `match` on them outside `id_effect_process` needs
a wildcard arm. See `docs/MIGRATION-0.11.md`, section "Processes".

## Scorecard

These close fault.actors rows #6 (timers and idle timeout), #7 (selective receive) and #8
(hibernate), each at level **documented**: implemented in `id_effect_process`, tested, and
described here. Tests: `crates/id_effect_process/tests/timers.rs`
(`send_after_delivers_under_test_clock`, `cancelled_timer_never_delivers`,
`timers_die_with_process`, `timer_into_full_mailbox_counts_against_bound`,
`idle_timeout_fires_once_per_idle_period`, `hibernate_releases_buffer_and_resumes`) and
`crates/id_effect_process/tests/selective_receive.rs`
(`selective_receive_keeps_order_of_skipped_messages`, `selective_receive_times_out`).

**Runnable examples:** `cargo run -p id_effect_process --example send_after_timers` and
`cargo run -p id_effect_process --example hibernate`.
