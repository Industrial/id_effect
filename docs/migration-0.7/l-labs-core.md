# Labs `id_effect_events` and `id_effect_parse` on the 0.7 kernel

0.7 moves the labs crate `id_effect_events` to the new kernel (`id_effect::next`). This is a
kernel migration only (R5): it adds no items and removes none. The public signatures differ only
in their kernel types:

| item | 0.6 | 0.7 |
|---|---|---|
| `EventStore::{append, read, latest_version}` | v1 `Effect<A, EventStoreError, ()>` | `next::Effect<A, EventStoreError>` (`R = HNil`) |
| `CommandHandler::handle` | v1 `Effect<Vec<Ev>, Self::Error, ()>` | `next::Effect<Vec<Ev>, Self::Error>` |
| `QueryHandler::query` | v1 `Effect<Ans, Self::Error, ()>` | `next::Effect<Ans, Self::Error>` |
| `MemoryEventStore`, `FileJournal`, `SqlEventJournal`, `EsEntityEventStore` | implement the v1 trait | implement the `next` trait |

`dispatch_command`, `query_projection`, `run_projection_from_store` (synchronous),
`ProjectionRunner::run_all` and `dispatch_command_es_entity` (async) keep their signatures. The
synchronous ones run on `next`'s blocking edge (`Effect::block_on` on `Runtime::global()`), so,
unlike v1 `run_blocking`, they must not be called from inside an async context (a tokio task):
there the run is refused and the call panics with the `BlockOnInAsync` defect. Use the async
APIs from async code. As before, a defect re-raises its panic.

```rust
// 0.6
use id_effect::{Effect, fail, run_blocking, succeed};
impl CommandHandler<Deposit, Evt> for DepositHandler {
  type Error = Reject;
  fn handle(&self, cmd: Deposit) -> Effect<Vec<Evt>, Reject, ()> {
    if cmd.amount <= 0 { return fail(Reject); }
    succeed(vec![Evt::Deposited(cmd.amount)])
  }
}
let stored = run_blocking(store.append("acct", &[Evt::Deposited(1)]), ())?;

// 0.7
use id_effect::next::{Effect, Runtime, fail, succeed};
impl CommandHandler<Deposit, Evt> for DepositHandler {
  type Error = Reject;
  fn handle(&self, cmd: Deposit) -> Effect<Vec<Evt>, Reject> {
    if cmd.amount <= 0 { return fail(Reject); }
    succeed(vec![Evt::Deposited(cmd.amount)])
  }
}
let stored = store.append("acct", &[Evt::Deposited(1)]).block_on().into_result()?; // sync
let stored = Runtime::global().run(store.read("acct", 1)).await.into_result()?; // async
```

- **An unmigrated (v1) caller** wraps a store call with the bridge:
  `use id_effect::next::compat::IntoV1; run_async(store.append(..).into_v1::<()>(), ())`.
- The es-entity `ProviderBox` providers (`provide_es_entity_events*`) are unchanged.

## `id_effect_parse`: unchanged

`id_effect_parse` uses `id_effect::schema` (kernel-independent, unchanged) and, in
`effect_bridge`, the v1 `Stream`. `next` has no `Stream` until `m-streams`, so `parse_stream` /
`parse_text_stream` keep taking a v1 `Stream<Chunk<_>, E, R>` and returning a v1
`Effect<_, ParseStreamError<E, P>, R>`. They move to the `next` stream when `k-retire-v1`
promotes `next` to the crate root.

## `id_effect_graph`: unchanged

`id_effect_graph` has no `id_effect` dependency.
