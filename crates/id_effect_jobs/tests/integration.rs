//! Cross-module integration: enqueue job while staging outbox event.

use id_effect::runtime::run_blocking;
use id_effect::succeed;
use id_effect_jobs::{
  JobRunner, JobSpec, MemoryJobRunner, MemoryOutbox, OutboxRecord, OutboxTable, drain_jobs,
  relay_outbox,
};

#[test]
fn job_and_outbox_together() {
  let runner = MemoryJobRunner::new();
  let outbox = MemoryOutbox::new();

  let job = run_blocking(runner.enqueue(JobSpec::new("notify", b"hello")), ()).unwrap();
  run_blocking(
    outbox.insert(OutboxRecord::new(
      job.spec.id.clone(),
      "JobEnqueued",
      job.spec.payload.clone(),
    )),
    (),
  )
  .unwrap();

  let processed = run_blocking(
    drain_jobs(runner.clone(), 1, |_spec| {
      succeed::<(), id_effect_jobs::JobError, ()>(())
    }),
    (),
  )
  .unwrap();
  assert_eq!(processed, 1);

  let relayed = run_blocking(
    relay_outbox(outbox.clone(), 10, |row| {
      assert_eq!(row.event_type, "JobEnqueued");
      succeed::<(), id_effect_jobs::OutboxError, ()>(())
    }),
    (),
  )
  .unwrap();
  assert_eq!(relayed, 1);

  assert_eq!(run_blocking(outbox.unpublished_count(), ()), Ok(0));
}
