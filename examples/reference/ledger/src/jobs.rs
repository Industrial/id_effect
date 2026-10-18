//! Background jobs on the PostgreSQL queue (`id_effect_jobs`, table `id_effect_jobs`): the
//! monthly statements and the mail they send.
//!
//! | Job | Queue | What |
//! |---|---|---|
//! | [`StatementsDue`] | `statements`, high priority | enqueued by the statement cron for the previous month; enqueues one [`MonthlyStatement`] per open account |
//! | [`MonthlyStatement`] | `statements` | unique per `account@month`: renders the CSV, stores it in S3, enqueues the mail |
//! | `DeliverMail` (id_effect_mail) | `mail` | sends a message through the [`MailerService`] |
//!
//! The cron runs on one instance at a time ([`cron_leader`]: a [`CronLeader`] over a
//! [`PgAdvisoryLease`]), and its entry ([`statements_cron`]) keys each tick's job
//! `statements.monthly@<tick>`, so even two leaders around a handover enqueue a tick once; the
//! per-account key makes a second fan-out of the same month a no-op as well. Every handler is
//! idempotent (delivery is at least once): a rerun overwrites the same S3 object and statement
//! row, and the mail job is unique per account and month.

use std::sync::Arc;
use std::time::Duration;

use id_effect::{
  Cron, CronRegistry, Effect, HCons, HNil, Never, Schedule, WallClock, fail, hlist, succeed,
};
use id_effect_kit::jobs::{
  ClusterSingleton, CronLeader, Job, JobError, JobFailure, JobOptions, JobQueueService,
  JobRegistry, NodeName, PgAdvisoryLease, PgJobQueue, Pid, Priority, UniqueKey, Worker, enqueue,
  enqueue_on_tick,
};
use id_effect_kit::mail::{
  Address, Attachment, DeliverMail, MailError, Mailbox, MailerService, register_mail_jobs,
};
use id_effect_kit::sql::{FromRow, SqlClient, SqlClientService, params};
use id_effect_kit::storage::{ObjectKey, ObjectStorage, SignedMethod};
use serde_json::json;

use crate::config::StatementsConfig;
use crate::db;
use crate::error::StartupError;
use crate::mail::{Mail, STATEMENT_MAIL};
use crate::statements::{self, Delivery, Line, Month};

/// The queue of the statements.
pub const STATEMENTS: &str = "statements";
/// The cron entry (and the prefix of its ticks' unique keys).
pub const STATEMENTS_CRON: &str = "statements.monthly";
/// The cluster singleton the statement cron's leader holds.
pub const CRON_LOCK: &str = "ledger.cron";

/// What the handlers run in: the queue (a handler enqueues follow-up jobs) and the mail
/// transport (`DeliverMail`).
pub type JobEnv = HCons<JobQueueService, HCons<MailerService, HNil>>;

/// The environment of the worker and the cron over `queue` and `mailer`.
pub fn job_env(queue: PgJobQueue, mailer: MailerService) -> JobEnv {
  hlist![JobQueueService::new(queue), mailer]
}

/// The PostgreSQL queue on `client`.
pub fn queue(client: &Arc<dyn SqlClient>) -> PgJobQueue {
  PgJobQueue::from_service(SqlClientService(Arc::clone(client)))
}

/// The statements of `month` are due: enqueue one [`MonthlyStatement`] per open account.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct StatementsDue {
  /// `YYYY-MM`.
  pub month: String,
}

impl Job for StatementsDue {
  const NAME: &'static str = "ledger.statements_due";
  const QUEUE: &'static str = STATEMENTS;

  /// Ahead of the statements already queued: the fan-out is one query.
  fn priority(&self) -> Priority {
    Priority::HIGH
  }

  fn max_attempts(&self) -> u16 {
    10
  }
}

/// The statement of one account for one month.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct MonthlyStatement {
  /// The account.
  pub account_id: i64,
  /// `YYYY-MM`.
  pub month: String,
}

impl Job for MonthlyStatement {
  const NAME: &'static str = "ledger.monthly_statement";
  const QUEUE: &'static str = STATEMENTS;

  fn max_attempts(&self) -> u16 {
    8
  }

  /// `account@month`, kept until the row is pruned: a month's statement is made once.
  fn unique_key(&self) -> Option<UniqueKey> {
    Some(UniqueKey::until_pruned(format!(
      "{}@{}",
      self.account_id, self.month
    )))
  }

  /// Exponential from 30 s, at most 30 minutes apart, jittered (S3 or the database may be
  /// briefly away).
  fn retry() -> Schedule<(), JobFailure> {
    Schedule::exponential(Duration::from_secs(30))
      .union(Schedule::spaced(Duration::from_secs(1800)))
      .jittered()
      .map(|_| ())
  }
}

/// What the statement handlers read and write.
#[derive(Clone)]
pub struct Statements {
  /// The primary (statements read what was committed).
  pub db: Arc<dyn SqlClient>,
  /// Where the CSVs go.
  pub storage: ObjectStorage,
  /// The statement mail's templates and sender.
  pub mail: Mail,
  /// Attach or link, and the link's lifetime.
  pub config: StatementsConfig,
}

/// Every job type with its handler: the two statement jobs and id_effect_mail's `DeliverMail`.
pub fn registry(statements: Statements) -> Result<JobRegistry<JobEnv>, JobError> {
  let db = Arc::clone(&statements.db);
  let statements = Arc::new(statements);
  let registry = JobRegistry::new()
    .register::<StatementsDue, JobFailure>(move |job, _ctx| fan_out(&db, job))?
    .register::<MonthlyStatement, JobFailure>(move |job, _ctx| Arc::clone(&statements).run(job))?;
  register_mail_jobs(registry)
}

/// A worker for the statements and the mail queue, `concurrency` jobs of each at a time.
pub fn worker(registry: JobRegistry<JobEnv>, concurrency: u16) -> Worker<JobEnv> {
  Worker::new(registry)
    .queue(STATEMENTS, concurrency)
    .queue(<DeliverMail as Job>::QUEUE, concurrency)
}

/// The statement cron: `cron` (six fields, UTC) enqueues [`StatementsDue`] for the month before
/// each tick.
pub fn statements_cron(cron: &str) -> Result<CronRegistry<JobEnv>, StartupError> {
  let cron =
    Cron::parse(cron).map_err(|e| StartupError::Settings(format!("statements.cron: {e}")))?;
  CronRegistry::new()
    .with(enqueue_on_tick(STATEMENTS_CRON, cron, |tick| {
      StatementsDue {
        month: Month::of(tick).previous().to_string(),
      }
    }))
    .map_err(|e| StartupError::Settings(format!("statements.cron: {e:?}")))
}

/// The cron's leader election on the database at `url`: a dedicated session holding the
/// advisory lock [`CRON_LOCK`] while this instance (`holder`) runs the schedule.
pub fn cron_leader(url: String, holder: String) -> Effect<CronLeader, StartupError> {
  PgAdvisoryLease::connect(url)
    .map_err(|e| StartupError::Settings(format!("cron lease: {e}")))
    .map(move |lease| {
      let pid = Pid::from_parts(NodeName::new(format!("{holder}@ledger")), 1, 1);
      let singleton =
        ClusterSingleton::new(CRON_LOCK, holder, Arc::new(lease), Duration::from_secs(60));
      CronLeader::new(singleton, pid)
    })
}

/// Runs `registry` while `leader` holds the lease, on the system clock, until interrupted.
pub fn run_cron(
  leader: &CronLeader,
  registry: &CronRegistry<JobEnv>,
  env: JobEnv,
) -> Effect<(), Never> {
  leader.run(registry, WallClock::system()).provide_all(env)
}

/// The fan-out: one [`MonthlyStatement`] per open account (a duplicate is a success).
fn fan_out(db: &Arc<dyn SqlClient>, job: StatementsDue) -> Effect<(), JobFailure, JobEnv> {
  if Month::parse(&job.month).is_none() {
    return fail(JobFailure::discard(format!("not a month: {}", job.month)));
  }
  db.query(
    "SELECT id FROM accounts WHERE closed_at IS NULL ORDER BY id",
    &[],
  )
  .map_err(JobFailure::from)
  .narrow()
  .flat_map(move |rows| {
    rows.iter().fold(succeed(()), |done, row| {
      let next = match row.get::<i64>(0usize) {
        Ok(account_id) => enqueue::<MonthlyStatement, JobEnv, _>(
          MonthlyStatement {
            account_id,
            month: job.month.clone(),
          },
          JobOptions::new(),
        )
        .map(|_| ())
        .map_err(JobFailure::retry),
        Err(e) => fail(JobFailure::from(e)),
      };
      done.flat_map(move |()| next)
    })
  })
}

/// The account a statement is for, with its owner's address.
#[derive(Clone, Debug, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct StatementAccount {
  org_id: String,
  code: String,
  name: String,
  owner_id: String,
  email: Option<String>,
}

#[derive(Clone, Debug, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct LineRow {
  date: String,
  entry_id: i64,
  memo: String,
  amount: i64,
}

#[derive(Clone, Debug, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct Opening {
  opening: i64,
}

const ACCOUNT: &str = "SELECT a.org_id, a.code, a.name, a.owner_id, u.email \
  FROM accounts a LEFT JOIN users u ON u.username = a.owner_id AND u.email_verified \
  WHERE a.id = $1";

const OPENING: &str = "SELECT COALESCE(SUM(p.amount), 0)::int8 AS opening \
  FROM postings p JOIN journal_entries e ON e.id = p.entry_id \
  WHERE p.account_id = $1 AND e.posted_at < $2::text::timestamptz";

const LINES: &str = "SELECT to_char(e.posted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS date, \
  e.id AS entry_id, e.memo, p.amount \
  FROM postings p JOIN journal_entries e ON e.id = p.entry_id \
  WHERE p.account_id = $1 AND e.posted_at >= $2::text::timestamptz \
    AND e.posted_at < $3::text::timestamptz \
  ORDER BY e.posted_at, e.id";

const RECORD: &str = "INSERT INTO statements (org_id, account_id, month, object_key, bytes, delivery) \
  VALUES ($1, $2, $3, $4, $5, $6) \
  ON CONFLICT (account_id, month) DO UPDATE SET object_key = EXCLUDED.object_key, \
    bytes = EXCLUDED.bytes, delivery = EXCLUDED.delivery";

/// A statement, built and stored: what its mail needs.
struct Built {
  account: StatementAccount,
  account_id: i64,
  month: Month,
  csv: Vec<u8>,
  opening: i64,
  closing: i64,
  postings: usize,
  delivery: Delivery,
  link: Option<String>,
}

impl Statements {
  /// The statement of `job`: CSV in S3, a `statements` row, and its mail enqueued.
  fn run(self: Arc<Self>, job: MonthlyStatement) -> Effect<(), JobFailure, JobEnv> {
    let Some(month) = Month::parse(&job.month) else {
      return fail(JobFailure::discard(format!("not a month: {}", job.month)));
    };
    let mail = Arc::clone(&self);
    self
      .build(job.account_id, month)
      .narrow()
      .flat_map(move |built| match mail.message(&built) {
        Ok(None) => succeed(()),
        Ok(Some(message)) => match DeliverMail::new(message) {
          Ok(deliver) => {
            let key = UniqueKey::until_pruned(format!("statement:{}@{}", built.account_id, month));
            enqueue::<DeliverMail, JobEnv, _>(deliver, JobOptions::new().unique(key))
              .map(|_| ())
              .map_err(JobFailure::retry)
          }
          Err(e) => fail(JobFailure::discard(e)),
        },
        Err(e) => fail(JobFailure::discard(e)),
      })
  }

  fn build(self: Arc<Self>, account_id: i64, month: Month) -> Effect<Built, JobFailure> {
    let this = Arc::clone(&self);
    db::query_one::<StatementAccount>(&self.db, ACCOUNT, params![account_id])
      .map_err(JobFailure::from)
      .flat_map(move |account| {
        let db = Arc::clone(&this.db);
        let start = month.start();
        let end = month.next().start();
        db::query_one::<Opening>(&db, OPENING, params![account_id, start.clone()])
          .flat_map(move |opening| {
            db::query_all::<LineRow>(&db, LINES, params![account_id, start, end])
              .map(move |rows| (account, opening.opening, rows))
          })
          .map_err(JobFailure::from)
          .flat_map(move |(account, opening, rows)| {
            this.store(account, account_id, month, opening, rows)
          })
      })
  }

  /// Renders the CSV, stores it, decides the delivery (signing the link) and records the row.
  fn store(
    self: Arc<Self>,
    account: StatementAccount,
    account_id: i64,
    month: Month,
    opening: i64,
    rows: Vec<LineRow>,
  ) -> Effect<Built, JobFailure> {
    let lines: Vec<Line> = rows
      .into_iter()
      .map(|r| Line {
        date: r.date,
        entry_id: r.entry_id,
        memo: r.memo,
        amount: r.amount,
      })
      .collect();
    let closing = opening + lines.iter().map(|l| l.amount).sum::<i64>();
    let csv = statements::csv(opening, &lines).into_bytes();
    let key = match ObjectKey::new(statements::object_key(&account.org_id, account_id, month)) {
      Ok(key) => key,
      Err(e) => return fail(JobFailure::discard(e)),
    };
    let delivery = Delivery::of(
      csv.len(),
      account.email.is_some(),
      self.config.attach_max_bytes,
    );
    let ttl = Duration::from_secs(self.config.link_ttl_hours * 3600);
    let storage = self.storage.clone();
    let db = Arc::clone(&self.db);
    let postings = lines.len();
    let put = storage
      .put(&key, csv.clone().into())
      .map_err(JobFailure::retry);
    let link = {
      let (storage, key) = (storage.clone(), key.clone());
      move |()| match delivery {
        Delivery::Linked => storage
          .signed_url(SignedMethod::Get, &key, ttl)
          .map(|url| Some(url.to_string()))
          .map_err(JobFailure::retry),
        _ => succeed(None),
      }
    };
    put.map(|_| ()).flat_map(link).flat_map(move |link| {
      let bytes = i64::try_from(csv.len()).unwrap_or(i64::MAX);
      db.execute(
        RECORD,
        &params![
          account.org_id.as_str(),
          account_id,
          month.to_string(),
          key.as_str(),
          bytes,
          delivery.as_str()
        ],
      )
      .map_err(JobFailure::from)
      .map(move |_| Built {
        account,
        account_id,
        month,
        csv,
        opening,
        closing,
        postings,
        delivery,
        link,
      })
    })
  }

  /// The statement mail of `built`, or `None` for an owner without a (verified) address.
  fn message(&self, built: &Built) -> Result<Option<id_effect_kit::mail::Message>, MailError> {
    let Some(email) = &built.account.email else {
      return Ok(None);
    };
    let to = Address::parse(email.clone())
      .map_err(|e| MailError::InvalidMessage(format!("owner address: {e}")))?;
    let filename = statements::file_name(&built.account.org_id, &built.account.code, built.month);
    let mut context = json!({
      "owner": built.account.owner_id,
      "org": built.account.org_id,
      "month": built.month.to_string(),
      "account": {"code": built.account.code, "name": built.account.name},
      "opening": statements::money(built.opening),
      "postings": built.postings,
      "closing": statements::money(built.closing),
      "filename": filename,
    });
    if let Some(link) = &built.link {
      context["link"] = json!(link);
      context["link_valid_for"] = json!(statements::in_words(Duration::from_secs(
        self.config.link_ttl_hours * 3600
      )));
    }
    let builder = self
      .mail
      .templates
      .message(STATEMENT_MAIL, &context)?
      .from(self.mail.from.clone())
      .to(Mailbox::named(built.account.owner_id.clone(), to));
    let builder = match built.delivery {
      Delivery::Attached => {
        builder.attach(Attachment::new(filename, "text/csv", built.csv.clone()))
      }
      _ => builder,
    };
    builder.build().map(Some)
  }
}
