# Two-way streams and resume

A two-way stream (`bidi`) takes a stream in and gives a stream out, on one call:

```rust,ignore
use id_effect::{DynEnv, Never, succeed};
use id_effect_rpc::{Duplex, Procedure, RpcGroup, WireSchema};

group.bidi(
  Procedure::bidi("Chat.room").guard(SignedIn),
  WireSchema::of::<Message>(),
  WireSchema::of::<Message>(),
  |_env: &mut DynEnv| {
    succeed::<_, Never, _>(Duplex::new(|incoming| incoming.map(moderate)))
  },
);
```

`Duplex::new` receives the client's items as an `id_effect` `Stream` and returns the stream it
answers with. It runs over WebSocket (chapter 33.6) and WebTransport. This chapter is about the
two things a long-lived stream needs on a real network: bounded memory when one side is slow,
and picking up where it left off when the connection drops.

## Backpressure in both directions

Each side may have only as many `item`s of a call outstanding as the other side granted it
credits: 32 at first, then whatever `credit` frames add.

- **Server to client.** The server sends an item only against a credit. When the credits are
  spent it stops pulling the handler's stream, so the producer suspends. A stream's memory is
  the credit window plus the call's small item queue, however fast the producer is.
- **Client to server.** The server queues at most the credits it granted and gives them back as
  the handler takes items, half a window at a time. A handler that stops reading stops the
  grants, and a client that sends past its credits fails the call with
  `ResourceExhausted { limit: "credits" }`.

A slow reader therefore slows only its own stream. The other calls on the socket keep going.

## Resume

A phone switching networks drops its socket. Without resume, every stream would restart from
the beginning. With it, the client reconnects and continues each stream after the last item it
saw.

### Cursors and sessions

Every server `item` of a server stream or a two-way stream carries a cursor:

```json
{"id": 3, "kind": "item", "cursor": "c1.AgAAAAAAAAAH", "payload": {"text": "hi"}}
```

The cursor is the item's position in the connection's **session**, a message sequence
(`Cursor::sequence(n)`). The session id came in the `hello` frame: 128 random bits, bound to the
principal the connection authenticated as.

The server keeps each item in the session's **replay buffer** until the client grants a credit
for it, which tells the server the client has it. So with a client that keeps its credits
flowing, the buffer holds at most one credit window per stream.

### Reconnecting

When the socket drops, the server-side streams keep running, detached. Their producers stop at
the credit window. The session waits for a resume for the retention period (30 s).

The client opens a new socket and, before it opens any call, sends `resume` with the session id
and, per call, the last cursor it saw (`c1.` of sequence 0 if it saw none):

```json
{"id": 0, "kind": "resume", "payload": {"session": "q1Fz…", "calls": [{"id": 3, "cursor": "c1.AgAAAAAAAAAH"}]}}
```

The server answers:

1. A `hello` naming the connection's session from now on: the resumed one, or the new socket's
   own one when the resume was refused.
2. For each call, the procedure's guards run again on the new socket's request. A cursor is not
   a capability: a token revoked since the first connection refuses the call with `Rejected`.
3. Then the items after the cursor, in order, and the `exit` if the call ended meanwhile. After
   that the call carries on live on the new socket.

The client sees every item once and in order. A property test checks this over 1,000 random
disconnect schedules (`resume_after_disconnect_no_gaps_no_duplicates`).

Credits carry over. The items at or before the cursor count as credited, so the client drops
the credits it had not yet sent for them. For a two-way stream, the first `credit` frame after
the resume sets the client's send window (it replaces the count, it does not add to it). The
client sends no item for that call before it. Client items that were in flight when the socket
dropped are not replayed: input is at most once across a reconnect. A protocol that needs
exactly-once input carries its own ids in its items.

### When resume says no

A call answers `exit` `ResumeExpired` (status 410), and the client restarts that stream, when:

- the buffer no longer holds what follows the cursor, because a bound evicted it;
- the session or the call is unknown, or the session was released;
- the session belongs to another principal.

The last case looks exactly like the others to the client, so a prober learns nothing about
whether the session exists. The server counts these refusals
(`RpcWsConfig::resume_stats().refused_principal`) and logs a warning.

## Bounds

```rust,ignore
use std::time::Duration;
use id_effect_rpc::resume::ResumeConfig;
use id_effect_rpc::transport::ws::RpcWsConfig;

let config = RpcWsConfig::default().resume(
  ResumeConfig::new()
    .max_messages(1024)            // frames per session
    .max_bytes(1024 * 1024)        // encoded bytes per session
    .retention(Duration::from_secs(30))
    .max_detached(1024),           // sessions waiting for a resume
);
```

These are the defaults. The oldest frames go first when a session is over a bound, and nothing
is kept longer than the retention period. A session detached for the retention period is
released and its streams are interrupted, which runs their finalizers. Past `max_detached`, the
session detached longest goes first. `ResumeConfig::max_messages(0)` turns resume off: every
resume expires.

Queries, mutations, client streams and subscriptions are not resumed. Unary calls and client
streams are interrupted with the socket. A subscription resumes from its event-log cursor in a
new `call` (chapter 33.8).

## Testing

`resume::TestClock` stands in for time, so retention is deterministic:

```rust,ignore
use std::time::Duration;
use id_effect_rpc::resume::{ResumeConfig, TestClock};

let clock = TestClock::new();
let config = RpcWsConfig::default().resume(ResumeConfig::new().clock(clock.clone()));
// … connect, open a stream, drop the socket …
clock.advance(Duration::from_secs(30));
assert_eq!(config.release_expired_sessions(), 1);
```

`resume::decode_cursor` and `resume::decode_resume` are the decoders of what a client sends.
Both are bounded (a cursor reads at most 64 bytes; a resume names at most 1,024 distinct calls)
and are fuzz targets.
