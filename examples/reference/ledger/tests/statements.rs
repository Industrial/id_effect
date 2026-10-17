//! Monthly statements (msg.jobs #2–#6 #8, msg.email #1 #2 #5 #8, web.storage #3 #5 #7): the
//! cron leader on a PostgreSQL advisory lock enqueues the month's `StatementsDue` once per tick,
//! the fan-out enqueues one `MonthlyStatement` per account (unique per `account@month`, however
//! many ticks fan out), each stores its CSV in S3 (SeaweedFS) and enqueues its mail with the CSV
//! attached; the mail queue delivers it to Mailpit. A statement that fails (S3 refuses a stale
//! secret) is retried on its schedule.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{mail_received, mailpit, mailpit_raw, run, s3, with_ledger};
use id_effect_kit::axum::EffectExecutor;
use id_effect_kit::jobs::{
  Job, JobFailure, JobFilter, JobOptions, JobQueue, JobState, Priority, enqueue, retry_delay,
};
use id_effect_kit::sql::params;
use id_effect_kit::storage::ObjectKey;
use id_effect_kit::test::ContainerError;
use ledger::jobs::{self, MonthlyStatement, Statements};
use ledger::mail::{self, Mail};
use ledger::statements::Month;
use serde_json::Value;

const BUCKET: &str = "ledger-statements";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_statement_cron_mails_every_account_its_csv_once() {
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let (mailpit, mailer) = mailpit().await;
  let (s3, storage, _) = s3(BUCKET).await;
  let (smtp, store) = (mailer.clone(), storage.clone());
  with_ledger(
    move |_, services| {
      services.mailer = smtp;
      services.storage = store;
    },
    move |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (cash, sales) = (&accounts[0], &accounts[1]);
      l.email("acme-accountant", "accountant@acme.example").await;
      l.transfer(&team.accountant, cash.id, sales.id, 300).await;
      l.transfer(&team.accountant, cash.id, sales.id, 50).await;
      // Last month's postings: the statement cron reports the month before its tick.
      l.execute(
        "UPDATE journal_entries SET posted_at = posted_at - interval '1 month'",
        vec![],
      )
      .await;

      // The cron, every second on the instance holding the lock, for a few ticks.
      let env = jobs::job_env(jobs::queue(&l.db), mailer.clone());
      let cron = jobs::statements_cron("* * * * * *").expect("a cron");
      let leader = run(jobs::cron_leader(l.url.clone(), "test-a".into())).await;
      let ticking = tokio::spawn(EffectExecutor::global().run_effect(jobs::run_cron(
        &leader,
        &cron,
        env.clone(),
      )));
      let due = "SELECT count(*)::int8 FROM id_effect_jobs WHERE name = 'ledger.statements_due'";
      for _ in 0..100 {
        if l.scalar(due, vec![]).await >= 2 {
          break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
      }
      ticking.abort();
      assert!(l.scalar(due, vec![]).await >= 2, "a job per tick");
      let queue = jobs::queue(&l.db);
      let ticks = run(queue.list(JobFilter::default())).await;
      for tick in &ticks {
        let key = tick.unique.as_ref().expect("a tick key").key.clone();
        assert!(key.starts_with("statements.monthly@"), "{key}");
        assert_eq!(tick.priority, Priority::HIGH);
        assert_eq!(tick.queue.as_str(), jobs::STATEMENTS);
      }

      // The worker: every tick fans out, but each account gets one statement.
      let month =
        Month::parse(ticks[0].payload["month"].as_str().expect("the month")).expect("a month");
      let statements = Statements {
        db: Arc::clone(&l.db),
        storage: storage.clone(),
        mail: Mail {
          mailer: mailer.clone(),
          templates: mail::templates(&l.config.mail).expect("templates"),
          from: mail::sender(&l.config.mail).expect("a sender"),
        },
        config: l.config.statements.clone(),
      };
      let worker = || jobs::worker(jobs::registry(statements.clone()).expect("handlers"), 2);
      run(worker().run_until_idle().provide_all(env.clone())).await;
      let per_account = run(queue.list(JobFilter::default()))
        .await
        .into_iter()
        .filter(|job| job.name == MonthlyStatement::NAME)
        .collect::<Vec<_>>();
      assert_eq!(per_account.len(), 2, "one statement per account");
      for job in &per_account {
        assert_eq!(job.state, JobState::Succeeded, "{job:?}");
        let key = &job.unique.as_ref().expect("a key").key;
        assert!(key.ends_with(&format!("@{month}")), "{key}");
      }

      // The CSV in S3, recorded as attached.
      let key = ObjectKey::new(format!("statements/acme/{}/{month}.csv", cash.id)).unwrap();
      let csv = run(storage.get(&key)).await;
      let csv = String::from_utf8(csv.to_vec()).expect("UTF-8");
      let lines: Vec<&str> = csv.lines().collect();
      assert_eq!(lines[0], "date,entry,memo,amount,balance");
      assert!(lines[1].ends_with(",transfer,300,300"), "{csv}");
      assert!(lines[2].ends_with(",transfer,50,350"), "{csv}");
      let delivered = l
        .scalar(
          "SELECT count(*)::int8 FROM statements WHERE delivery = 'attached' AND month = $1",
          params![month.to_string()],
        )
        .await;
      assert_eq!(delivered, 2);

      // The mail: rendered from the templates, the CSV attached, delivered by the mail queue.
      let received = mail_received(&mailpit, 2).await;
      let to_cash = received
        .iter()
        .find(|m| m["Subject"].as_str().unwrap().contains(&cash.code))
        .expect("the cash account's statement");
      assert_eq!(to_cash["To"][0]["Address"], "accountant@acme.example");
      assert_eq!(
        to_cash["Subject"],
        format!("Your {month} statement for {} {}", cash.code, cash.name)
      );
      assert!(
        to_cash["Text"]
          .as_str()
          .unwrap()
          .contains("Closing balance  3.50")
      );
      let attachment = &to_cash["Attachments"][0];
      assert_eq!(
        attachment["FileName"],
        format!("acme-{}-{month}.csv", cash.code)
      );
      let id = to_cash["ID"].as_str().unwrap();
      let part = attachment["PartID"].as_str().unwrap();
      let attached = mailpit_raw(&mailpit, &format!("/api/v1/message/{id}/part/{part}")).await;
      assert_eq!(attached, csv);

      // A second month-end run (another tick, a restarted leader) adds nothing.
      run(worker().run_until_idle().provide_all(env)).await;
      assert_eq!(mail_received(&mailpit, 2).await.len(), 2);
      remove(mailpit.container()).await;
    },
  )
  .await;
  remove(s3.container()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_statement_that_cannot_be_stored_is_retried() {
  if !id_effect_kit::test::require_docker() {
    return;
  }
  // The instance's S3 credentials are wrong (a rotated secret): the first attempt fails and is
  // scheduled again; once they are right, the retry stores the statement.
  let (mailpit, mailer) = mailpit().await;
  let (s3, storage, mut rotated) = s3(BUCKET).await;
  rotated.secret_access_key = Some(id_effect_kit::config::Secret::new("rotated".to_owned()));
  let stale = ledger::app::statement_store(&rotated).expect("a store");
  with_ledger(
    |_, _| {},
    move |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      l.transfer(&team.accountant, accounts[0].id, accounts[1].id, 10)
        .await;
      let month = Month::parse("2026-10").unwrap();
      let env = jobs::job_env(jobs::queue(&l.db), mailer.clone());
      let statements = |storage| Statements {
        db: Arc::clone(&l.db),
        storage,
        mail: Mail {
          mailer: mailer.clone(),
          templates: mail::templates(&l.config.mail).expect("templates"),
          from: mail::sender(&l.config.mail).expect("a sender"),
        },
        config: l.config.statements.clone(),
      };
      let queue = jobs::queue(&l.db);
      run(
        enqueue::<MonthlyStatement, jobs::JobEnv, _>(
          MonthlyStatement {
            account_id: accounts[0].id,
            month: month.to_string(),
          },
          JobOptions::new(),
        )
        .provide_all(env.clone()),
      )
      .await;
      let worker = |statements| jobs::worker(jobs::registry(statements).expect("handlers"), 1);
      run(
        worker(statements(stale))
          .run_until_idle()
          .provide_all(env.clone()),
      )
      .await;
      let failed = run(queue.list(JobFilter::default())).await.remove(0);
      assert_eq!((failed.state, failed.attempts), (JobState::Failed, 1));
      assert!(failed.last_error.is_some());
      // On its schedule: 30 s (jittered) after the first failure.
      let delay =
        retry_delay(&MonthlyStatement::retry(), &JobFailure::retry("x"), 1).expect("a retry");
      assert!(
        delay >= Duration::from_secs(20) && delay <= Duration::from_secs(40),
        "{delay:?}"
      );

      // The secret is right again; the retry (brought forward) stores the statement.
      l.execute(
        "UPDATE id_effect_jobs SET run_at = now() WHERE state = 'failed'",
        vec![],
      )
      .await;
      run(
        worker(statements(storage))
          .run_until_idle()
          .provide_all(env),
      )
      .await;
      let done = run(queue.list(JobFilter::default()))
        .await
        .into_iter()
        .find(|job| job.name == MonthlyStatement::NAME)
        .expect("the statement");
      assert_eq!((done.state, done.attempts), (JobState::Succeeded, 2));
      // No owner address: stored, not mailed.
      let stored = l
        .scalar(
          "SELECT count(*)::int8 FROM statements WHERE delivery = 'stored'",
          vec![],
        )
        .await;
      assert_eq!(stored, 1);
      let messages: Value = common::mailpit_api(&mailpit, "/api/v1/messages").await;
      assert_eq!(messages["total"], 0);
      remove(s3.container()).await;
      remove(mailpit.container()).await;
    },
  )
  .await;
}

async fn remove(container: &id_effect_kit::test::Container) {
  let _ = EffectExecutor::global()
    .run_effect(container.remove::<ContainerError, id_effect::HNil>())
    .await;
}
