# Bounded subprocess pool

**Problem.** You have N external commands to run (compilers, linters, test shards) and the machine
can only afford K of them at once. A common hand-rolled answer is a `Mutex` + `Condvar` counter
around `std::process::Command`. That works, but it blocks threads, is hard to test, and ignores the
effect runtime you already have.

**Recipe.** Start every job concurrently and let a core `Semaphore` with K permits decide which
ones run. Each job:

1. waits for a permit (`Semaphore::acquire_owned`),
2. runs its command through the `ProcessRuntimeService` capability (`id_effect_platform::command`),
3. gives the permit back when the permit value is dropped. That happens on success, failure and
   cancellation alike.

```rust
use id_effect::{Effect, Needs, Semaphore, box_future, run_async};
use id_effect_platform::command::{CommandSpec, ProcessRuntimeService, spawn_wait};
use id_effect_platform::error::ProcessError;

fn bounded_job<R>(sem: Semaphore, cmd: CommandSpec) -> Effect<i32, ProcessError, R>
where
  R: Needs<ProcessRuntimeService> + 'static,
{
  Effect::new_async(move |r: &mut R| {
    let run = spawn_wait::<R>(cmd);
    box_future(async move {
      // Waits while all K permits are taken; `_permit` releases on drop.
      let _permit = match run_async(sem.acquire_owned(), ()).await {
        Ok(permit) => permit,
        Err(never) => match never {},
      };
      let status = run.run(r).await?;
      Ok(status.code().unwrap_or(-1))
    })
  })
}
```

At the edge, build the semaphore and the capability once, then start all jobs. The example uses a
current-thread Tokio runtime with a `LocalSet`, so nothing has to be `Send`:

```rust
let sem = run_blocking(Semaphore::make(3), ()).expect("semaphore");
let env = build_env([provide!(TokioProcessRuntimeProvider)]).expect("env");

let local = tokio::task::LocalSet::new();
let codes: Vec<i32> = local.run_until(async {
  let handles: Vec<_> = commands.into_iter()
    .map(|cmd| tokio::task::spawn_local(run_async(bounded_job(sem.clone(), cmd), env.clone())))
    .collect();
  let mut codes = Vec::new();
  for h in handles { codes.push(h.await.expect("join").expect("spawn")); }
  codes
}).await;
```

The runnable example counts how many jobs are inside the permit at once and asserts the peak never
exceeds K. To stay portable, its "external command" is the example binary itself, re-invoked as
`child <exit-code>`.

Full example: [`crates/id_effect_platform/examples/152_cookbook_bounded_command_pool.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_platform/examples/152_cookbook_bounded_command_pool.rs)

```bash
cargo run -p id_effect_platform --example 152_cookbook_bounded_command_pool
```

## Queue or reject?

| You want | Use |
|----------|-----|
| surplus work **waits** for a slot (a build pool, a batch job) | core `Semaphore` (this recipe) |
| surplus work is **rejected** immediately (protect a hot path, shed load) | `id_effect_resilience::Bulkhead::with_permit`, which fails with `Or::Left(BulkheadError)` when full |

Because `ProcessRuntimeService` is a capability, tests can provide a fake runtime that records
`CommandSpec`s instead of spawning processes. The pool logic does not change.

See also: [Platform I/O](../part2/ch07-06-platform-services.md),
[Runtime resilience](../part5/ch21-00-runtime-resilience.md).
