//! Delivery through `id_effect_jobs` (feature `jobs`): `deliver_later` and the worker on a
//! `MemoryJobQueue` with a `TestMailbox` on virtual time, and `deliver_later_in` on a live
//! PostgreSQL transaction.
//!
//! The PostgreSQL test needs a database at `DATABASE_URL`. With `ID_EFFECT_REQUIRE_PG=1` a missing
//! or unreachable database is a test **failure**; without it the test prints an explicit `SKIP`
//! line to stderr and returns. It runs in a schema of its own (`search_path`).

#![cfg(feature = "jobs")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::{
  Cause, Effect, Executor, Exit, HCons, HNil, Never, TestExecutor, UtcDateTime, WallClock, hlist,
};
use id_effect_jobs::{
  DrainReport, Job, JobError, JobFilter, JobQueue, JobQueueService, JobRecord, JobRegistry,
  JobState, MemoryJobQueue, PgJobQueue, Worker, WorkerConfig, migrator,
};
use id_effect_mail::{
  Address, Attachment, DeliverMail, MailError, MailerService, Message, TestMailbox, deliver_later,
  deliver_later_in, register_mail_jobs,
};
use id_effect_sql::SqlClient;
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
use id_effect_tokio::TokioRuntime;

type Env = HCons<JobQueueService, HCons<MailerService, HNil>>;

fn invoice(to: &str) -> Message {
  Message::builder()
    .from(Address::parse("billing@ledger.example").unwrap())
    .to(Address::parse(to).unwrap())
    .subject("Invoice 7")
    .text("Your invoice is attached.")
    .attach(Attachment::new(
      "invoice.pdf",
      "application/pdf",
      b"%PDF-1.7".to_vec(),
    ))
    .build()
    .unwrap()
}

fn worker(registry: JobRegistry<Env>, clock: WallClock) -> Worker<Env> {
  // One attempt at a time: queued mailbox failures meet the jobs in enqueue order.
  Worker::new(registry)
    .queue(DeliverMail::QUEUE, 1)
    .with_config(WorkerConfig {
      clock,
      ..WorkerConfig::default()
    })
}

/// A memory queue and a mailbox on a test executor's virtual time.
struct Memory {
  exec: TestExecutor,
  store: MemoryJobQueue,
  mailbox: TestMailbox,
  worker: Worker<Env>,
}

impl Memory {
  fn new() -> Self {
    let exec = TestExecutor::new();
    let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_800_000_000_000));
    let registry = register_mail_jobs(JobRegistry::<Env>::new()).unwrap();
    Self {
      store: MemoryJobQueue::new(clock.clone()),
      mailbox: TestMailbox::new(),
      worker: worker(registry, clock),
      exec,
    }
  }

  fn env(&self) -> Env {
    hlist![
      JobQueueService::new(self.store.clone()),
      MailerService::new(self.mailbox.clone())
    ]
  }

  fn drive<A: Send + 'static>(&self, eff: Effect<A, JobError>) -> A {
    let slot = Arc::new(Mutex::new(None));
    let out = Arc::clone(&slot);
    let done = eff
      .exit::<Never>()
      .map(move |exit| *out.lock().unwrap() = Some(exit))
      .fork_root(Arc::new(self.exec.clone()));
    self.exec.run_until_stalled();
    assert!(done.is_done(), "finished without advancing time");
    match slot.lock().unwrap().take() {
      Some(Exit::Success(a)) => a,
      Some(Exit::Failure(Cause::Fail(e))) => panic!("failed: {e}"),
      Some(Exit::Failure(cause)) => panic!("unexpected cause: {cause:?}"),
      None => panic!("no exit"),
    }
  }

  fn deliver_later(&self, message: Message) {
    let enqueued = self.drive(deliver_later::<Env, _>(message).provide_all(self.env()));
    assert!(enqueued.is_new());
  }

  fn run_until_idle(&self) -> DrainReport {
    self.drive(self.worker.clone().run_until_idle().provide_all(self.env()))
  }

  /// Every job, oldest first.
  fn jobs(&self) -> Vec<JobRecord> {
    let mut all = self.drive(self.store.list(JobFilter::default()));
    all.reverse();
    all
  }
}

#[test]
fn deliver_later_retries_a_transient_failure_then_sends() {
  let m = Memory::new();
  m.mailbox.fail_next(MailError::Temporary {
    code: 451,
    message: "greylisted".into(),
  });
  m.deliver_later(invoice("carol@ledger.example"));
  m.mailbox.assert_none_sent();

  let first = m.run_until_idle();
  assert_eq!((first.retrying, first.runs()), (1, 1));
  m.mailbox.assert_none_sent();
  let failed = &m.jobs()[0];
  assert_eq!(
    (failed.name.as_str(), failed.queue.as_str()),
    (DeliverMail::NAME, "mail")
  );
  assert_eq!((failed.state, failed.attempts), (JobState::Failed, 1));
  assert_eq!(
    failed.last_error.as_deref(),
    Some("mail temporarily refused (451): greylisted")
  );

  // Not before its backoff (30 s ± 20 %)…
  m.exec.advance(Duration::from_secs(20));
  assert_eq!(m.run_until_idle().runs(), 0);
  // …and sent once it is due.
  m.exec.advance(Duration::from_secs(20));
  let second = m.run_until_idle();
  assert_eq!((second.succeeded, second.runs()), (1, 1));
  let sent = m.mailbox.assert_sent_to("carol@ledger.example");
  assert_eq!(
    sent,
    invoice("carol@ledger.example"),
    "the message as enqueued"
  );
  assert_eq!(sent.attachments()[0].data(), b"%PDF-1.7");
  let done = &m.jobs()[0];
  assert_eq!((done.state, done.attempts), (JobState::Succeeded, 2));
}

#[test]
fn deliver_later_permanent_failure_goes_dead_without_retries() {
  let m = Memory::new();
  let permanent = [
    MailError::Rejected {
      code: 550,
      message: "no such user".into(),
    },
    MailError::InvalidMessage("bad header".into()),
    MailError::Unauthorized("bad API key".into()),
  ];
  for error in &permanent {
    m.mailbox.fail_next(error.clone());
    m.deliver_later(invoice("nobody@ledger.example"));
  }

  let report = m.run_until_idle();
  assert_eq!((report.dead, report.runs()), (3, 3));
  // Dead is final: an hour later nothing runs again.
  m.exec.advance(Duration::from_secs(3600));
  assert_eq!(m.run_until_idle().runs(), 0);
  m.mailbox.assert_none_sent();
  let jobs = m.jobs();
  for (job, error) in jobs.iter().zip(&permanent) {
    assert_eq!((job.state, job.attempts), (JobState::Dead, 1));
    assert_eq!(job.last_error.as_deref(), Some(error.to_string().as_str()));
    assert!(job.finished_at.is_some());
  }
}

#[test]
fn deliver_later_refuses_content_over_the_payload_limit() {
  let m = Memory::new();
  let big = Message::builder()
    .from(Address::parse("billing@ledger.example").unwrap())
    .to(Address::parse("carol@ledger.example").unwrap())
    .text("Attached.")
    .attach(Attachment::new(
      "export.csv",
      "text/csv",
      vec![b'x'; DeliverMail::MAX_CONTENT_BYTES + 1],
    ))
    .build()
    .unwrap();
  let slot = Arc::new(Mutex::new(None));
  let out = Arc::clone(&slot);
  deliver_later::<Env, _>(big)
    .provide_all(m.env())
    .exit::<Never>()
    .map(move |exit| *out.lock().unwrap() = Some(exit))
    .fork_root(Arc::new(m.exec.clone()));
  m.exec.run_until_stalled();
  let exit = slot.lock().unwrap().take();
  assert!(
    matches!(exit, Some(Exit::Failure(Cause::Fail(JobError::Encode(_))))),
    "{exit:?}"
  );
  assert!(m.jobs().is_empty(), "nothing enqueued");
}

// ── PostgreSQL ──────────────────────────────────────────────────────────────

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

/// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
fn unavailable<T>(test: &str, why: &str) -> Option<T> {
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

struct Db {
  rt: TokioRuntime,
  client: PgSqlClient,
}

impl Db {
  /// A client on the fresh schema `mail_<test>`, with the jobs table migrated.
  fn new(test: &str) -> Option<Self> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) => url,
      Err(_) => return unavailable(test, "DATABASE_URL is not set"),
    };
    let rt = TokioRuntime::new_multi_thread().expect("tokio runtime");
    let schema = format!("mail_{test}");
    let admin = match rt.block_on(pg_pool_from_config(
      PgPoolConfig::from_url(url.clone()).with_max_size(1),
    )) {
      Ok(pool) => PgSqlClient::new(pool),
      Err(e) => return unavailable(test, &format!("cannot connect: {}", e.0)),
    };
    let create = format!("CREATE SCHEMA {schema}");
    let fresh = admin
      .execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"), &[])
      .flat_map(move |_| admin.execute(&create, &[]));
    if let Exit::Failure(c) = rt.block_on(rt.run(fresh)) {
      return unavailable(test, &format!("{c:?}"));
    }
    let sep = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{sep}options=-c%20search_path%3D{schema}");
    let pool = rt
      .block_on(pg_pool_from_config(
        PgPoolConfig::from_url(scoped).with_max_size(4),
      ))
      .expect("pool on the test schema");
    let db = Self {
      rt,
      client: PgSqlClient::new(pool),
    };
    let applied = db.run(
      migrator()
        .run_on(Arc::new(db.client.clone()))
        .map_err(|e| e.to_string()),
    );
    assert_eq!(applied, [1]);
    Some(db)
  }

  fn run<A: Send + 'static, E: std::fmt::Debug + Send + 'static>(&self, eff: Effect<A, E>) -> A {
    match self.rt.block_on(self.rt.run(eff)) {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("failed: {c:?}"),
    }
  }

  fn jobs(&self) -> i64 {
    self.run(
      self
        .client
        .query("SELECT count(*) FROM id_effect_jobs", &[]),
    )[0]
      .get::<i64>(0usize)
      .expect("a count")
  }
}

#[test]
fn deliver_later_sends_after_commit_only() {
  let test = "deliver_later_sends_after_commit_only";
  let Some(db) = Db::new(test) else {
    return;
  };
  let clock = WallClock::system();
  let mailbox = TestMailbox::new();
  let env: Env = hlist![
    JobQueueService::new(PgJobQueue::new(db.client.clone())),
    MailerService::new(mailbox.clone())
  ];
  let registry = register_mail_jobs(JobRegistry::<Env>::new()).unwrap();
  let run = || {
    db.run(
      worker(registry.clone(), clock.clone())
        .run_until_idle()
        .provide_all(env.clone()),
    )
  };

  // Rolled back: no job, nothing sent.
  let tx = db.run(db.client.begin());
  let enqueued = db.run(deliver_later_in(&tx, invoice("eve@ledger.example"), &clock));
  assert!(enqueued.is_new());
  db.run(tx.rollback());
  assert_eq!(db.jobs(), 0);
  assert_eq!(run().runs(), 0);
  mailbox.assert_none_sent();

  // Committed: sent, but not while the transaction is open.
  let tx = db.run(db.client.begin());
  db.run(deliver_later_in(
    &tx,
    invoice("carol@ledger.example"),
    &clock,
  ));
  assert_eq!(run().runs(), 0, "the worker cannot see an uncommitted job");
  mailbox.assert_none_sent();
  db.run(tx.commit());
  let report = run();
  assert_eq!((report.succeeded, report.runs()), (1, 1));
  mailbox.assert_count(1);
  let sent = mailbox.assert_sent_to("carol@ledger.example");
  assert_eq!(sent, invoice("carol@ledger.example"));
  let state: Vec<String> = db
    .run(db.client.query("SELECT state FROM id_effect_jobs", &[]))
    .iter()
    .map(|r| r.get::<String>(0usize).expect("a text column"))
    .collect();
  assert_eq!(state, [JobState::Succeeded.as_str()]);
  assert_eq!(DeliverMail::QUEUE, "mail");
}
