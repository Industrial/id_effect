# Bounded subprocess pool

**Problem.** You have N external commands to run (compilers, linters, test shards) and the machine
can only afford K of them at once. A common hand-rolled answer is a `Mutex` + `Condvar` counter
around `std::process::Command`. That works, but it blocks threads, is hard to test, and ignores the
effect runtime you already have.

**Recipe.** Fork every job at once with `for_each_par` and let a core `Semaphore` with K permits
decide which ones run. Each job runs its command through the `ProcessRuntimeService` capability
(`id_effect_platform::command`) inside `sem.with_permit(..)`, which waits for a permit and releases
it however the job ends: success, failure or interruption.

```rust
use id_effect::{Concurrency, Effect, Exit, Semaphore, for_each_par};
use id_effect_platform::command::{
  CommandSpec, ProcessRuntimeEnv, TokioProcessRuntimeProvider, spawn_wait,
};
use id_effect_platform::error::ProcessError;
use id_effect_tokio::TokioRuntime;

/// One job: run `cmd` holding a permit, yield the exit code.
fn bounded_job(sem: &Semaphore, cmd: CommandSpec) -> Effect<i32, ProcessError, ProcessRuntimeEnv> {
  let job = spawn_wait(cmd).map(|status| status.code().unwrap_or(-1));
  sem.with_permit(job) // waits while all K permits are taken
}

// Called from `#[tokio::main(flavor = "current_thread")] async fn main()`:
async fn run_pool(commands: Vec<CommandSpec>) {
  let sem = Semaphore::new(3);
  let jobs = for_each_par(
    commands,
    |cmd| bounded_job(&sem, cmd),
    Concurrency::Unbounded, // the semaphore, not the fork policy, bounds the pool
  );
  // Capabilities are provided once, at the edge, by the process layer.
  let program = jobs.provide_layer(TokioProcessRuntimeProvider::layer());
  let codes: Vec<i32> = match TokioRuntime::current().expect("tokio").run(program).await {
    Exit::Success(codes) => codes,
    Exit::Failure(cause) => panic!("jobs failed: {cause:?}"),
  };
}
# // No commands here, so the doctest spawns no processes.
# TokioRuntime::new_current_thread().expect("tokio").block_on(run_pool(Vec::new()));
```

`for_each_par` returns the results in input order. The runnable example counts how many jobs are
inside the permit at once and asserts the peak never exceeds K. To stay portable, its "external
command" is the example binary itself, re-invoked as `child <exit-code>`.

`Concurrency::Bounded(k)` on `for_each_par` would also cap the pool. The semaphore is the better
fit when the limit must be shared by jobs started from different places.

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
