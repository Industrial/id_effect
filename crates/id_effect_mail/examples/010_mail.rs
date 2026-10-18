//! Sends a mail in the background: `deliver_later` enqueues it as a `DeliverMail` job, a worker
//! sends it through the `Mailer`, retries a temporary refusal after its backoff, and gives up at
//! once on a permanent one. Runs on an in-memory queue, a `TestMailbox` and virtual time.
//!
//! `cargo run -p id_effect_mail --example 010_mail --features jobs`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{
  Cause, Effect, Executor, Exit, HCons, HNil, Never, TestExecutor, UtcDateTime, WallClock, hlist,
};
use id_effect_jobs::{
  DrainReport, Job, JobError, JobFilter, JobQueue, JobQueueService, JobRegistry, MemoryJobQueue,
  Worker, WorkerConfig,
};
use id_effect_mail::{
  Address, DeliverMail, MailError, MailerService, Message, TestMailbox, deliver_later,
  register_mail_jobs,
};

type Env = HCons<JobQueueService, HCons<MailerService, HNil>>;

/// Runs `eff` on `exec` until it stalls; the effect must not wait on time.
fn drive<A: Send + 'static>(exec: &TestExecutor, eff: Effect<A, JobError>) -> A {
  let slot = Arc::new(Mutex::new(None));
  let out = Arc::clone(&slot);
  let _done = eff
    .exit::<Never>()
    .map(move |exit| *out.lock().unwrap() = Some(exit))
    .fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  match slot.lock().unwrap().take() {
    Some(Exit::Success(a)) => a,
    Some(Exit::Failure(Cause::Fail(e))) => panic!("failed: {e}"),
    Some(Exit::Failure(cause)) => panic!("failed: {cause:?}"),
    None => panic!("did not finish"),
  }
}

fn invoice(to: &str) -> Message {
  Message::builder()
    .from(Address::parse("billing@ledger.example").unwrap())
    .to(Address::parse(to).unwrap())
    .subject("Your invoice")
    .text("Your invoice is attached.")
    .build()
    .unwrap()
}

fn main() {
  let exec = TestExecutor::new();
  let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_800_000_000_000));
  let store = MemoryJobQueue::new(clock.clone());
  let mailbox = TestMailbox::new();
  let env: Env = hlist![
    JobQueueService::new(store.clone()),
    MailerService::new(mailbox.clone())
  ];
  let registry = register_mail_jobs(JobRegistry::<Env>::new()).unwrap();
  // One attempt at a time, so the queued mailbox failures meet the jobs in enqueue order.
  let worker = Worker::new(registry)
    .queue(DeliverMail::QUEUE, 1)
    .with_config(WorkerConfig {
      clock,
      ..WorkerConfig::default()
    });
  let run = |exec: &TestExecutor| -> DrainReport {
    drive(
      exec,
      worker.clone().run_until_idle().provide_all(env.clone()),
    )
  };

  // The provider greylists the first attempt, then accepts; the second address does not exist.
  mailbox.fail_next(MailError::Temporary {
    code: 451,
    message: "greylisted, try again later".into(),
  });
  mailbox.fail_next(MailError::Rejected {
    code: 550,
    message: "no such user".into(),
  });
  for to in ["carol@ledger.example", "nobody@ledger.example"] {
    drive(
      &exec,
      deliver_later::<Env, _>(invoice(to)).provide_all(env.clone()),
    );
  }

  let first = run(&exec);
  println!(
    "first pass: {} retrying, {} dead, {} sent",
    first.retrying,
    first.dead,
    mailbox.len()
  );
  exec.advance(Duration::from_secs(60));
  let second = run(&exec);
  println!(
    "after 60 s: {} succeeded, {} sent",
    second.succeeded,
    mailbox.len()
  );

  let mut jobs = drive(&exec, store.list(JobFilter::default()));
  jobs.reverse();
  for job in &jobs {
    println!(
      "job {} {}: {} after {} attempt(s){}",
      job.id,
      job.name,
      job.state,
      job.attempts,
      job
        .last_error
        .as_deref()
        .map(|e| format!(", last error: {e}"))
        .unwrap_or_default()
    );
  }
  mailbox.assert_sent_to("carol@ledger.example");
  assert_eq!(mailbox.len(), 1);
}
