//! Account closure as a durable workflow (fault.workflows #2 #3 #4 #5 #7 on the reference app):
//! the approval signal, the 30-day grace timer under a test clock, the final statement exported
//! by an activity on the job queue, and the saga's compensation, across a restart onto another
//! pool, engine and app instance.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use common::{As, connect, run, s3, with_ledger};
use id_effect::{UtcDateTime, WallClock};
use id_effect_kit::jobs::{JobRegistry, Worker, WorkerConfig};
use id_effect_kit::mail::{MailerService, TestMailbox};
use id_effect_kit::sql::SqlClient;
use id_effect_kit::storage::{ObjectKey, ObjectStorage};
use id_effect_kit::test::http::{StatusCode, TestClient};
use id_effect_kit::workflow::{ACTIVITY_QUEUE, TIMER_QUEUE, WorkflowId, WorkflowStatus};
use ledger::closure::{AccountClosure, Closures, GRACE, final_statement_key};
use ledger::jobs::{self, JobEnv};
use ledger::{Database, Services};
use serde_json::{Value, json};

const BUCKET: &str = "ledger-closures";

/// A clock that starts now and moves only when the test jumps it.
fn test_clock() -> WallClock {
  let now = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock");
  let now = UtcDateTime::unsafe_make(i64::try_from(now.as_millis()).expect("millis"));
  WallClock::anchored(Instant::now(), now)
}

/// One Ledger instance's workflow side: its engine and the worker running the closure
/// activities and timers, on `db` and `storage`, under `clock`.
struct Instance {
  closures: Closures,
  db: Arc<dyn SqlClient>,
  registry: JobRegistry<JobEnv>,
  clock: WallClock,
}

impl Instance {
  fn new(db: Arc<dyn SqlClient>, storage: &ObjectStorage, clock: &WallClock) -> Self {
    let closures = Closures::new(&db, clock.clone()).expect("the closure engine");
    let registry = closures
      .register(JobRegistry::new(), &db, storage)
      .expect("the closure activities");
    Self {
      closures,
      db,
      registry,
      clock: clock.clone(),
    }
  }

  /// Runs every runnable activity and due timer once.
  async fn drain(&self) {
    let worker = Worker::new(self.registry.clone())
      .queue(ACTIVITY_QUEUE, 2)
      .queue(TIMER_QUEUE, 1)
      .with_config(WorkerConfig {
        clock: self.clock.clone(),
        ..WorkerConfig::default()
      });
    let queue = jobs::queue(&self.db).with_clock(self.clock.clone());
    let env = jobs::job_env(queue, MailerService::new(TestMailbox::new()));
    run(worker.run_until_idle().provide_all(env)).await;
  }

  /// Resumes `id` and drains the queue until it parks on something no worker can deliver (a
  /// signal, a timer not yet due) or finishes.
  async fn advance(&self, id: &WorkflowId) -> WorkflowStatus {
    let engine = self.closures.engine();
    for _ in 0..20 {
      run(engine.resume(id)).await;
      let record = run(engine.instance(id)).await.expect("the instance");
      if record.status.is_terminal() {
        return record.status;
      }
      let before = record.wakeups;
      self.drain().await;
      let after = run(engine.instance(id))
        .await
        .expect("the instance")
        .wakeups;
      if record.waiting && after == before {
        return record.status;
      }
    }
    panic!("{id} did not settle");
  }
}

/// `GET /accounts/{account}/closure` on `client` as `who`.
async fn closure_of(
  l: &common::Ledger,
  client: &TestClient<axum::Router>,
  who: &As,
  account: i64,
) -> Value {
  let res = l
    .send(l.by(who, client.get(&format!("/accounts/{account}/closure"))))
    .await;
  res.assert_status(StatusCode::OK);
  res.json().expect("json")
}

#[tokio::test]
async fn account_closure_workflow_survives_restart() {
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let (_s3, storage, _) = s3(BUCKET).await;
  let store = storage.clone();
  with_ledger(
    move |_, services| services.storage = store,
    move |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (cash, sales) = (accounts[0].id, accounts[1].id);
      l.transfer(&team.accountant, cash, sales, 300).await;
      let clock = test_clock();

      // The accountant who opened the account asks for its closure.
      let asked = l
        .post(
          &format!("/accounts/{cash}/closure"),
          json!({}),
          &team.accountant,
        )
        .await;
      asked.assert_status(StatusCode::ACCEPTED);
      let id =
        WorkflowId::new(asked.json::<Value>().unwrap()["instance"].as_str().unwrap()).unwrap();
      l.post(
        &format!("/accounts/{cash}/closure"),
        json!({}),
        &team.accountant,
      )
      .await
      .assert_status(StatusCode::CONFLICT);

      // Instance A freezes the account and parks for the approval.
      let first = Instance::new(Arc::clone(&l.db), &storage, &clock);
      assert_eq!(first.advance(&id).await, WorkflowStatus::Running);
      let account: Value = l
        .get(&format!("/accounts/{cash}"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(account["locked"], true, "frozen: {account}");
      let parked = closure_of(&l, &l.client, &team.viewer, cash).await;
      assert_eq!(
        (parked["status"].clone(), parked["waiting"].clone()),
        (json!("running"), json!(true))
      );
      // Locked: another accountant no longer posts to it (its opener and the owners still do).
      l.user("acme", "acme-clerk", "accountant", None).await;
      let clerk = l.login("acme-clerk", None).await;
      l.post(
        "/entries",
        json!({"memo": "late", "lines": [
          {"account_id": cash, "amount": 1}, {"account_id": sales, "amount": -1},
        ]}),
        &clerk,
      )
      .await
      .assert_status(StatusCode::FORBIDDEN);
      drop(first);

      // Restart: another app instance, pool and engine on the same database. The owner
      // approves through it; the requester may not decide its own closure.
      let pool = connect(&l.url).await;
      let services = Services {
        storage: storage.clone(),
        ..Services::new(Database::new(Arc::clone(&pool), vec![]))
      };
      let second_app = common::behind_proxy(ledger::router(&l.config, services).expect("the app"));
      let second_app = TestClient::new(second_app);
      let approval = format!("/accounts/{cash}/closure/approval");
      l.send(l.by(
        &team.accountant,
        second_app.post_json(&approval, &json!({"approved": true})),
      ))
      .await
      .assert_status(StatusCode::FORBIDDEN);
      l.send(l.by(
        &team.owner,
        second_app.post_json(&approval, &json!({"approved": true})),
      ))
      .await
      .assert_status(StatusCode::ACCEPTED);
      let second = Instance::new(pool, &storage, &clock);
      assert_eq!(second.advance(&id).await, WorkflowStatus::Running);

      // The grace period: a durable timer, not yet due. The owner moves the balance out.
      let instance = run(second.closures.engine().instance(&id)).await.unwrap();
      assert!(instance.waiting, "waiting out the grace period");
      l.send(l.by(
        &team.owner,
        second_app.post_json(
          "/entries",
          &json!({"memo": "move out", "lines": [
            {"account_id": sales, "amount": 300}, {"account_id": cash, "amount": -300},
          ]}),
        ),
      ))
      .await
      .assert_status(StatusCode::CREATED);
      second.drain().await;
      assert!(
        run(second.closures.engine().instance(&id))
          .await
          .unwrap()
          .waiting
      );

      // Thirty days later the timer fires, the final statement is exported, the account closed.
      clock.jump(GRACE + Duration::from_secs(1));
      assert_eq!(second.advance(&id).await, WorkflowStatus::Completed);
      let closed = run(second.closures.engine().result::<AccountClosure>(&id))
        .await
        .expect("the output");
      assert_eq!(closed.approved_by, "acme-owner");
      assert_eq!(closed.statement, final_statement_key("acme", cash));
      l.get(&format!("/accounts/{cash}"), &team.viewer)
        .await
        .assert_status(StatusCode::NOT_FOUND);
      let done = closure_of(&l, &second_app, &team.viewer, cash).await;
      assert_eq!(done["status"], "completed", "{done}");

      let key = ObjectKey::new(final_statement_key("acme", cash)).unwrap();
      let csv = String::from_utf8(run(storage.get(&key)).await.to_vec()).expect("UTF-8");
      let lines: Vec<&str> = csv.lines().collect();
      assert_eq!(lines.len(), 3, "{csv}");
      assert!(lines[1].ends_with(",transfer,300,300"), "{csv}");
      assert!(lines[2].ends_with(",move out,-300,0"), "{csv}");
    },
  )
  .await;
}

#[tokio::test]
async fn a_rejected_closure_thaws_the_account() {
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let (_s3, storage, _) = s3(BUCKET).await;
  let store = storage.clone();
  with_ledger(
    move |_, services| services.storage = store,
    move |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 1).await;
      let cash = accounts[0].id;
      let clock = test_clock();
      let instance = Instance::new(Arc::clone(&l.db), &storage, &clock);

      let asked = l
        .post(
          &format!("/accounts/{cash}/closure"),
          json!({}),
          &team.accountant,
        )
        .await;
      asked.assert_status(StatusCode::ACCEPTED);
      let id =
        WorkflowId::new(asked.json::<Value>().unwrap()["instance"].as_str().unwrap()).unwrap();
      assert_eq!(instance.advance(&id).await, WorkflowStatus::Running);
      l.post(
        &format!("/accounts/{cash}/closure/approval"),
        json!({"approved": false}),
        &team.owner,
      )
      .await
      .assert_status(StatusCode::ACCEPTED);

      // The saga's compensation unlocks the account; the instance fails with the rejection.
      assert_eq!(instance.advance(&id).await, WorkflowStatus::Failed);
      let account: Value = l
        .get(&format!("/accounts/{cash}"), &team.viewer)
        .await
        .json()
        .unwrap();
      assert_eq!(account["locked"], false, "thawed: {account}");
      let failed = closure_of(&l, &l.client, &team.viewer, cash).await;
      assert_eq!(failed["status"], "failed");
      assert!(
        failed["error"]
          .as_str()
          .unwrap()
          .contains("rejected by acme-owner"),
        "{failed}"
      );
      let history = run(instance.closures.engine().store().history(&id)).await;
      let names: Vec<&str> = history.iter().map(|e| e.name.as_str()).collect();
      assert_eq!(
        names,
        [
          "ledger.closure.freeze",
          "compensate:ledger.closure.thaw",
          "approval",
          "ledger.closure.thaw"
        ]
      );

      // A rejected closure can be asked for again.
      l.post(
        &format!("/accounts/{cash}/closure"),
        json!({}),
        &team.accountant,
      )
      .await
      .assert_status(StatusCode::ACCEPTED);
    },
  )
  .await;
}
