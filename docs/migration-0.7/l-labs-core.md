# Labs `id_effect_events` and `id_effect_parse` on the 0.7 kernel

0.7 moves the labs crate `id_effect_events` to the new kernel. This is a
kernel migration only (R5): it adds no items and removes none. The public signatures differ only
in their kernel types:

| item | 0.6 | 0.7 |
|---|---|---|
| `EventStore::{append, read, latest_version}` | v1 `Effect<A, EventStoreError, ()>` | `Effect<A, EventStoreError>` (`R = HNil`) |
| `CommandHandler::handle` | v1 `Effect<Vec<Ev>, Self::Error, ()>` | `Effect<Vec<Ev>, Self::Error>` |
| `QueryHandler::query` | v1 `Effect<Ans, Self::Error, ()>` | `Effect<Ans, Self::Error>` |
| `MemoryEventStore`, `FileJournal`, `SqlEventJournal`, `EsEntityEventStore` | implement the v1 trait | implement the trait |

`dispatch_command`, `query_projection`, `run_projection_from_store` (synchronous),
`ProjectionRunner::run_all` and `dispatch_command_es_entity` (async) keep their signatures. The
synchronous ones run on the kernel's blocking edge (`Effect::block_on` on `Runtime::global()`), so,
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
use id_effect::{Effect, Runtime, fail, succeed};
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
  `use id_effect::compat::IntoV1; run_async(store.append(..).into_v1::<()>(), ())`.
- The es-entity providers (`provide_es_entity_events*`) are `Layer`s since k-retire-v1
  (last section).

## `id_effect_parse`: schema bridge unchanged

`id_effect_parse` uses `id_effect::schema` (kernel-independent, unchanged). Its `effect_bridge`
first stayed on the v1 `Stream` (the 0.7 kernel had no `Stream` before `m-streams`); the follow-up below
moves it.

## `id_effect_parse`: stream bridge on `Stream` (follow-up)

With `m-streams` landed, `effect_bridge` moves to the 0.7 `Stream` ahead of `k-retire-v1`.
`parse_stream` / `parse_text_stream` and `ParseStreamError` keep their names and shapes; only the
kernel types change (R5, nothing added or removed):

| item | 0.6 | 0.7 |
|---|---|---|
| `parse_stream` input | v1 `Stream<Chunk<I>, E, R>` | `Stream<Chunk<I>, E, R>` |
| `parse_stream` output | v1 `Effect<O, ParseStreamError<E, P>, R>` | `Effect<O, ParseStreamError<E, P>, R>` |
| `parse_text_stream` input / output | v1 `Stream<Chunk<u8>, E, R>` / v1 `Effect<..>` | `Stream<Chunk<u8>, E, R>` / `Effect<..>` |
| bound on `R` | `R: 'static` | `R: Send + Sync + 'static` (the 0.7 `Stream` bound) |

`Chunk` is the same type (`id_effect` re-exports it). Stream failures still surface as
`ParseStreamError::Stream(e)`; in 0.7 they arrive as the typed failure of the `Exit`'s `Cause`.

```rust
// 0.6
use id_effect::{Chunk, Stream, run_blocking};
let stream = Stream::from_iterable(vec![Chunk::from_vec(b"hi".to_vec())]);
let value = run_blocking(parse_text_stream(tag("hi"), stream), ())?;

// 0.7
use id_effect::{Chunk, Stream};
let stream = Stream::<Chunk<u8>>::from_iterable(vec![Chunk::from_vec(b"hi".to_vec())]);
let value = parse_text_stream(tag("hi"), stream).block_on().into_result(); // Result<_, Cause<_>>
let err = value.unwrap_err().fail_option(); // Some(ParseStreamError::..) on a typed failure
```

- `Stream`'s error defaults to `Never`, so a stream that cannot fail needs only the item
  type (`Stream::<Chunk<u8>>`); E otherwise has no inference source.
- **A v1 caller** holding a v1 `Stream` has no stream bridge; build the input as a `Stream`
  and convert the result with `id_effect::compat::IntoV1` (`.into_v1::<()>()` for
  `R = HNil`) if a v1 `Effect` is needed. No labs crate calls these functions.

## `id_effect_graph`: unchanged

`id_effect_graph` has no `id_effect` dependency.

## `id_effect_events` (feature `es-entity`): the journal providers are `Layer`s (k-retire-v1)

With the v1 capability DI retired, `provide_es_entity_events` and
`provide_es_entity_events_from_pool` keep their names and arguments but return a
`Layer` instead of a `ProviderBox`. The capability is still `EventJournalBackend`
(`Arc<EsEntityPgBackend>`); the new alias `EventJournalEnv = HCons<EventJournalBackend, HNil>` names
the layers' output. They need nothing (`In = HNil`) and never fail, so `E` is the error type of
the graph they join.

| 0.6 | 0.7 |
|---|---|
| `provide_es_entity_events(backend) -> ProviderBox` | `provide_es_entity_events(backend) -> Layer<EventJournalEnv, E>` |
| `provide_es_entity_events_from_pool(pool) -> ProviderBox` | `provide_es_entity_events_from_pool(pool) -> Layer<EventJournalEnv, E>` |
| `Needs::<EventJournalBackend>::need(&env)` | `need::<EventJournalBackend>()` in an effect over `R: Selector<EventJournalBackend, I>` |

```rust
// 0.6
let env = build_env([provide_es_entity_events_from_pool(pool)])?;
let backend = Needs::<EventJournalBackend>::need(&env).clone();

// 0.7
use id_effect::{Effect, need};
let program: Effect<_, MyError, EventJournalEnv> =
  need::<EventJournalBackend>().flat_map(|slot| use_backend(EventJournalBackend::clone(&slot)));
let exit = program.provide_layer(provide_es_entity_events_from_pool(pool)).block_on();
```
