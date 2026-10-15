//! Cross-module integration: enqueue job while staging outbox event.

use id_effect::next::{Effect, succeed};
use id_effect_jobs::{
  JobRunner, JobSpec, MemoryJobRunner, MemoryOutbox, OutboxRecord, OutboxTable, drain_jobs,
  relay_outbox,
};

fn run<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Result<A, E> {
  eff
    .block_on()
    .into_result()
    .map_err(|c| c.fail_option().expect("typed failure"))
}

#[test]
fn job_and_outbox_together() {
  let runner = MemoryJobRunner::new();
  let outbox = MemoryOutbox::new();

  let job = run(runner.enqueue(JobSpec::new("notify", b"hello"))).unwrap();
  run(outbox.insert(OutboxRecord::new(
    job.spec.id.clone(),
    "JobEnqueued",
    job.spec.payload.clone(),
  )))
  .unwrap();

  let processed = run(drain_jobs(runner.clone(), 1, |_spec| {
    succeed::<(), id_effect_jobs::JobError, _>(())
  }))
  .unwrap();
  assert_eq!(processed, 1);

  let relayed = run(relay_outbox(outbox.clone(), 10, |row| {
    assert_eq!(row.event_type, "JobEnqueued");
    succeed::<(), id_effect_jobs::OutboxError, _>(())
  }))
  .unwrap();
  assert_eq!(relayed, 1);

  assert_eq!(run(outbox.unpublished_count()), Ok(0));
}
