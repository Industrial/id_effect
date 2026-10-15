# Labs `id_effect_workflow` and `id_effect_jobs` on the 0.7 kernel

0.7 moves the labs crates `id_effect_workflow` and `id_effect_jobs` to the new kernel
(`id_effect`). This is a kernel migration only (R5): no items are added or removed, and the
public signatures differ only in their kernel types (and the `Send` bounds the 0.7 kernel needs).
`id_effect::compute` (`FabricJobSpec`, `WorkProfile`, …) is kernel-neutral and unchanged.

## `id_effect_jobs`

| item | 0.6 | 0.7 |
|---|---|---|
| `JobRunner::{enqueue, dequeue, complete, fail, pending_count}` | v1 `Effect<A, JobError, ()>` | `Effect<A, JobError>` (`R = HNil`) |
| `FabricJobRunner::{enqueue_fabric, dequeue_for, pending_for}` | v1 `Effect<A, JobError, ()>` | `Effect<A, JobError>` |
| `OutboxTable::{insert, fetch_unpublished, mark_published, unpublished_count}` | v1 `Effect<A, OutboxError, ()>` | `Effect<A, OutboxError>` |
| `MessageBroker::publish` | v1 `Effect<(), JobError, ()>` | `Effect<(), JobError>` |
| `JobQueue::enqueue`, `ApalisJobQueue::enqueue` | v1 `Effect<JobRecord, JobError, ()>` | `Effect<JobRecord, JobError>` |
| `ObixOutbox::insert` / `ObixInbox::persist` | v1 `Effect<_, _, ()>` | `Effect<_, _>` |
| `drain_jobs(runner, limit, handler)` | `handler: Fn(&JobSpec) -> v1 Effect + Copy + 'static`; nested `run_blocking` per job | `handler: Fn(&JobSpec) -> Effect + Copy + Send + 'static`; one effect, no thread parking |
| `relay_outbox(outbox, limit, publish)` | `publish: Fn(&OutboxRecord) -> v1 Effect + 'static`; nested `run_blocking` | `publish: … -> Effect + Send + 'static`; one effect |

`MemoryJobRunner`, `MemoryOutbox`, `MemoryBroker`, `KafkaBrokerStub`, `RdKafkaBroker` implement the
Traits. `drain_jobs` still marks a job failed on a **typed** handler failure and goes on; a
handler defect or interrupt now propagates out of the drain (0.6 `run_blocking` panicked). The
sqlx-backed effects (apalis, obix) need a tokio runtime with its IO driver: run them with
`id_effect_tokio::TokioRuntime` (or `Runtime::global()`).

```rust
// 0.6
use id_effect::{run_async, run_blocking, succeed};
let rec = run_blocking(runner.enqueue(JobSpec::new("a", b"1")), ())?;
let n = run_blocking(drain_jobs(runner.clone(), 10, |_| succeed::<(), JobError, ()>(())), ())?;
let row = run_async(obix.insert(record), ()).await?;

// 0.7
use id_effect::succeed;
use id_effect_tokio::TokioRuntime;
let rec = runner.enqueue(JobSpec::new("a", b"1")).block_on().into_result()?; // sync edge
let n = drain_jobs(runner.clone(), 10, |_| succeed::<(), JobError, _>(()))
  .block_on()
  .into_result()?;
let row = TokioRuntime::current()?.run(obix.insert(record)).await.into_result()?; // async
```

- **A v1 caller** wraps a call with the bridge:
  `use id_effect::compat::IntoV1; run_async(runner.dequeue().into_v1::<()>(), ())`.
- `.into_result()` gives `Result<A, Cause<E>>`; `cause.fail_option()` is the typed failure.

### `apalis` feature: fixed against the locked `apalis-postgres` 1.0.0

The `apalis` adapter did not compile at 0.6 HEAD: it was written for `apalis-postgres`
1.0.0-rc.8 (`PostgresStorage<Args, Compact, Codec>`, `Config::new`, `new_with_config`, sqlx 0.8),
while `labs/Cargo.lock` resolves `version = "1"` to 1.0.0, which is on **sqlx 0.9**. It now uses
the 1.0.0 API (`PostgresStorage::<()>::setup`, `PostgresStorage::new(pool).with_config(..)`), and
`ApalisJobQueue::{setup, new, pool}` take/return `apalis_postgres::PgPool` (sqlx 0.9) instead of
`sqlx::PgPool` (0.8). No dependency or lockfile change. The obix adapters stay on sqlx 0.8, so a
program using both opens two pools on the same database:

```rust
let pool = pg_pool_from_config(PgPoolConfig::from_url(url.clone())).await?; // sqlx 0.8: obix
let apalis_pool = apalis_postgres::PgPool::connect(&url).await?;          // sqlx 0.9: apalis
ApalisJobQueue::setup(&apalis_pool).await?;
let queue = ApalisJobQueue::new(&apalis_pool, "demo");
```

## `id_effect_workflow`

| item | 0.6 | 0.7 |
|---|---|---|
| `provide_duroxide_pg(pool, url)` | v1 `ProviderBox` (`Cap<DuroxideProvider>`, `Cap<WorkflowRuntime>` in a v1 `Env`) | `provide_duroxide_pg::<E>(pool, url) -> Layer<HCons<DuroxideProvider, HCons<WorkflowRuntime, HNil>>, E>` (`Layer::succeed`; the caller owns the pool) |
| `impl StepJournal for DuroxideStepJournal`, `DuroxideStepJournal::migrate_sync` | tokio `block_in_place` + `Handle::block_on` (needed a multi-thread tokio runtime, called **from inside** it) | the blocking edge: each statement runs on a fiber of `Runtime::global()` while the calling thread parks; call it **from a plain thread** |

`StepJournal`, `DurableWorkflowLog` (SQLite), `NetworkJournalStub`, `RemoteStepJournal`,
`JournalServer`, the `fsm` helpers (`register_fsm`, `step_durable`, `restore_state`) and
`bootstrap_duroxide_schema` (async) are unchanged: they are synchronous and use no effect.

**Behaviour change.** The synchronous `DuroxideStepJournal` methods are a blocking edge, like
every blocking edge: inside an async context (a tokio task, `spawn_blocking`, an entered
runtime) the run is refused and the call panics with the `BlockOnInAsync` defect. In 0.6 the same
call had to be made inside a multi-thread tokio runtime. Move the journal work to a thread. A
`run_step_typed` transaction still spans the lookup, `compute` and the insert; a failed or
panicking `compute` rolls it back.

```rust
// 0.6 (inside #[tokio::main] / #[tokio::test(flavor = "multi_thread")])
let mut journal = DuroxideStepJournal::new(pool);
journal.register_workflow(&wf)?;
let step: String = journal.run_step_typed(&wf, 0, "s", || Ok("done".into()))?;

// 0.7: off the async runtime
let step = std::thread::spawn(move || {
  let mut journal = DuroxideStepJournal::new(pool);
  journal.register_workflow(&wf)?;
  journal.run_step_typed::<String, _>(&wf, 0, "s", || Ok("done".into()))
})
.join()
.expect("journal thread")?;

// providers: 0.6 build_env([provide_duroxide_pg(pool, url)])?
let program = program.provide_layer(provide_duroxide_pg::<AppError>(pool, url));
```

## Tests and examples

- The pg tests (`id_effect_jobs` `tests/pg_integration.rs`, `id_effect_workflow`
  `tests/duroxide_journal.rs`) follow the `id_effect_sql` pattern: with `ID_EFFECT_REQUIRE_PG=1`
  a missing database **fails** them; without it they print `SKIP`.
- obix runs no migrations of its own, so `obix_outbox_smoke` applies obix 0.2.33's schema from
  `tests/fixtures/obix_0.2.33_setup.sql` when `persistent_outbox_events` is missing, and lets
  `job::Jobs::init` run job's migrations (`exec_migrations(true)`). At 0.6 the test needed a
  database prepared by hand.
- `apalis-postgres` checks its `query_file!` macros against the database when `DATABASE_URL` is
  set at **compile** time, which fails on a database without the `apalis` schema (a fresh dev
  database: `relation "apalis.jobs" does not exist`). Build the pg features
  with `SQLX_OFFLINE=true` (the crate ships its offline query data) and keep `DATABASE_URL` for the
  test run:
  `devenv shell -- env SQLX_OFFLINE=true ID_EFFECT_REQUIRE_PG=1 cargo nextest run --manifest-path labs/Cargo.toml -p id_effect_jobs -p id_effect_workflow --all-features`.
- Example `020_events_workflow_e2e` no longer needs the `into_v1` adapter (events and jobs are both
  on the 0.7 kernel); it runs its journal step on a thread. Verified against the devenv database.
- Example `030_compute_fabric_cluster` asserted that a saturated node drops to exactly 1 permit;
  the (kernel-neutral) governor floors at half the permits unless the CPU spread bucket is empty,
  so the example panicked (on that assertion, before any jobs code runs). It now asserts the node
  is throttled below its maximum.
