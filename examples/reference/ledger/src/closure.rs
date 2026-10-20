//! Account closure as a durable workflow (`id_effect_workflow`, the kit's `workflow` feature).
//!
//! `DELETE /accounts/{id}` closes an empty account at once. `POST /accounts/{id}/closure` starts
//! [`AccountClosure`] instead: a workflow instance journaled in PostgreSQL, which survives
//! restarts and runs on whichever instance's engine claims it.
//!
//! | Step | What | Undone by |
//! |---|---|---|
//! | [`Freeze`] (activity) | locks the account: only its owner or an organisation owner still posts (to move the balance out) | [`Thaw`]: the lock it had before |
//! | signal [`APPROVAL`] | an organisation owner other than the requester approves or rejects (`POST /accounts/{id}/closure/approval`) | — |
//! | timer | [`GRACE`] (30 days), a job with `run_at`, not a sleeping task | — |
//! | [`ExportStatement`] (activity) | the account's final statement (every posting) to `statements/<org>/<account>/final.csv` | — |
//! | [`CloseAccount`] (activity) | closes the account; refused while it holds a balance | — |
//!
//! The body is a saga: a rejection, or a closure the account's balance refuses, fails the
//! instance and runs the compensation [`Thaw`] (a journaled activity, at least once). Activities
//! and the timer are jobs of the Ledger's queue (`workflow.activities`, `workflow.timers`), run
//! by the worker of any instance ([`crate::background`]); every activity is idempotent.
//!
//! One instance per closure request: its id is `closure:<org>:<account>:<version>` (the
//! account's version when it was asked), so a rejected closure can be asked again. The tables
//! are `migrations/0008_workflows.up.sql` (a copy of id_effect_workflow's).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use id_effect::{Effect, HList, WallClock, fail, succeed};
use id_effect_kit::authz::Policies;
use id_effect_kit::axum::RequestCaps;
use id_effect_kit::axum::route::{Input, Route, Router};
use id_effect_kit::jobs::{JobFailure, JobRegistry};
use id_effect_kit::sql::{FromRow, SqlClient, SqlClientService, params};
use id_effect_kit::storage::{ObjectKey, ObjectStorage};
use id_effect_kit::workflow::{
  Activity, ActivityContext, PgWorkflowStore, Workflow, WorkflowContext, WorkflowEngine,
  WorkflowError, WorkflowId,
};
use serde::Serialize;

use crate::accounts::{AccountId, find_account};
use crate::app::{App, AppEnv, as_caller, guarded};
use crate::error::ApiError;
use crate::statements::{self, Line};
use crate::{db, jobs};

/// How long an approved closure waits before the account is closed.
pub const GRACE: Duration = Duration::from_secs(30 * 24 * 3600);

/// The signal an owner's decision is sent as.
pub const APPROVAL: &str = "approval";

/// What a closure is about, journaled as the instance's input.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct Closure {
  /// The organisation.
  pub org_id: String,
  /// The account.
  pub account_id: i64,
  /// Who asked for it (who may not approve it).
  pub requested_by: String,
  /// Whether the account was locked before the closure locked it ([`Thaw`] restores it).
  pub was_locked: bool,
}

/// An owner's decision ([`APPROVAL`]).
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct Approval {
  /// Close the account (after [`GRACE`]), or keep it.
  pub approved: bool,
  /// Who decided.
  pub by: String,
}

/// Where [`ExportStatement`] stored the final statement.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct FinalStatement {
  /// The object key.
  pub object_key: String,
  /// Its size.
  pub bytes: i64,
}

/// The output of a completed closure.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct Closed {
  /// The closed account.
  pub account_id: i64,
  /// Who approved it.
  pub approved_by: String,
  /// Where its final statement is.
  pub statement: String,
}

/// The workflow: freeze, wait for the approval, wait out the grace period, export the final
/// statement, close; a failure after the freeze thaws the account.
pub struct AccountClosure;

impl Workflow for AccountClosure {
  type Input = Closure;
  type Output = Closed;
  const NAME: &'static str = "ledger.account-closure";

  fn run(&self, ctx: WorkflowContext, closure: Closure) -> Effect<Closed, WorkflowError> {
    let (decide, grace, export, close) = (ctx.clone(), ctx.clone(), ctx.clone(), ctx.clone());
    ctx.saga(move |saga| {
      saga
        .activity::<Freeze, Thaw, _>(closure, Closure::clone)
        .flat_map(move |closure| {
          decide
            .wait_signal::<Approval>(APPROVAL)
            .map(move |approval| (closure, approval))
        })
        .flat_map(move |(closure, approval)| {
          if !approval.approved {
            return fail(WorkflowError::failed(format!(
              "the closure of account {} was rejected by {}",
              closure.account_id, approval.by
            )));
          }
          grace.sleep(GRACE).map(move |()| (closure, approval))
        })
        .flat_map(move |(closure, approval)| {
          export
            .activity::<ExportStatement>(closure.clone())
            .map(move |statement| (closure, approval, statement))
        })
        .flat_map(move |(closure, approval, statement)| {
          close
            .activity::<CloseAccount>(closure)
            .map(move |closed| Closed {
              account_id: closed.account_id,
              approved_by: approval.by,
              statement: statement.object_key,
            })
        })
    })
  }
}

const FREEZE: &str = "UPDATE accounts SET locked = true, version = version + 1 \
  WHERE id = $1 AND org_id = $2 AND closed_at IS NULL AND NOT locked";

const IS_OPEN: &str = "SELECT count(*)::int8 FROM accounts \
  WHERE id = $1 AND org_id = $2 AND closed_at IS NULL";

const THAW: &str = "UPDATE accounts SET locked = $3, version = version + 1 \
  WHERE id = $1 AND org_id = $2 AND closed_at IS NULL AND locked <> $3";

const FINAL_LINES: &str = "SELECT to_char(e.posted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS date, \
  e.id AS entry_id, e.memo, p.amount \
  FROM postings p JOIN journal_entries e ON e.id = p.entry_id \
  WHERE p.org_id = $1 AND p.account_id = $2 \
  ORDER BY e.posted_at, e.id";

/// Closes the account in the statement that checks its balance, so a posting cannot slip in
/// between the check and the close.
const CLOSE: &str = "UPDATE accounts a SET closed_at = now(), version = a.version + 1 \
  WHERE a.id = $1 AND a.org_id = $2 AND a.closed_at IS NULL \
    AND (SELECT COALESCE(SUM(p.amount), 0) FROM postings p WHERE p.account_id = a.id) = 0";

const BALANCE: &str = "SELECT COALESCE(SUM(amount), 0)::int8 AS balance, \
  (SELECT count(*)::int8 FROM accounts WHERE id = $2 AND org_id = $1 AND closed_at IS NULL) \
    AS open \
  FROM postings WHERE org_id = $1 AND account_id = $2";

/// Locks the account (idempotent: an account already locked stays as it is).
pub struct Freeze(pub Arc<dyn SqlClient>);

impl Activity for Freeze {
  type Input = Closure;
  type Output = Closure;
  const NAME: &'static str = "ledger.closure.freeze";

  fn run(&self, _: ActivityContext, closure: Closure) -> Effect<Closure, JobFailure> {
    let db = Arc::clone(&self.0);
    let (id, org) = (closure.account_id, closure.org_id.clone());
    self
      .0
      .execute(FREEZE, &params![id, org.as_str()])
      .flat_map(move |_| db::query_one::<(i64,)>(&db, IS_OPEN, params![id, org.as_str()]))
      .map_err(JobFailure::from)
      .flat_map(move |(open,)| {
        if open == 0 {
          return fail(JobFailure::discard(format!(
            "account {id} is closed or does not exist"
          )));
        }
        succeed(closure)
      })
  }
}

/// Restores the lock the account had before the closure (the compensation of [`Freeze`]).
pub struct Thaw(pub Arc<dyn SqlClient>);

impl Activity for Thaw {
  type Input = Closure;
  type Output = Closure;
  const NAME: &'static str = "ledger.closure.thaw";

  fn run(&self, _: ActivityContext, closure: Closure) -> Effect<Closure, JobFailure> {
    let params = params![
      closure.account_id,
      closure.org_id.as_str(),
      closure.was_locked
    ];
    self
      .0
      .execute(THAW, &params)
      .map(move |_| closure)
      .map_err(JobFailure::from)
  }
}

#[derive(Clone, Debug, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct LineRow {
  date: String,
  entry_id: i64,
  memo: String,
  amount: i64,
}

/// The account's final statement: every posting, from a zero opening balance, stored under
/// `statements/<org>/<account>/final.csv` (a rerun overwrites the same object).
pub struct ExportStatement {
  /// The primary.
  pub db: Arc<dyn SqlClient>,
  /// The statements bucket.
  pub storage: ObjectStorage,
}

/// Where the final statement of `account` in `org` is stored.
pub fn final_statement_key(org: &str, account: i64) -> String {
  format!("statements/{org}/{account}/final.csv")
}

impl Activity for ExportStatement {
  type Input = Closure;
  type Output = FinalStatement;
  const NAME: &'static str = "ledger.closure.export";

  fn run(&self, _: ActivityContext, closure: Closure) -> Effect<FinalStatement, JobFailure> {
    let key = match ObjectKey::new(final_statement_key(&closure.org_id, closure.account_id)) {
      Ok(key) => key,
      Err(e) => return fail(JobFailure::discard(e)),
    };
    let storage = self.storage.clone();
    let params = params![closure.org_id.as_str(), closure.account_id];
    db::query_all::<LineRow>(&self.db, FINAL_LINES, params)
      .map_err(JobFailure::from)
      .flat_map(move |rows| {
        let lines: Vec<Line> = rows
          .into_iter()
          .map(|r| Line {
            date: r.date,
            entry_id: r.entry_id,
            memo: r.memo,
            amount: r.amount,
          })
          .collect();
        let csv = statements::csv(0, &lines).into_bytes();
        let bytes = i64::try_from(csv.len()).unwrap_or(i64::MAX);
        storage
          .put(&key, csv.into())
          .map(move |_| FinalStatement {
            object_key: key.as_str().to_owned(),
            bytes,
          })
          .map_err(JobFailure::retry)
      })
  }
}

/// Closes the account; a final failure while it holds a balance (or is gone), which fails the
/// closure and thaws it.
pub struct CloseAccount(pub Arc<dyn SqlClient>);

impl Activity for CloseAccount {
  type Input = Closure;
  type Output = Closure;
  const NAME: &'static str = "ledger.closure.close";

  fn run(&self, _: ActivityContext, closure: Closure) -> Effect<Closure, JobFailure> {
    let db = Arc::clone(&self.0);
    let (id, org) = (closure.account_id, closure.org_id.clone());
    self
      .0
      .execute(CLOSE, &params![id, org.as_str()])
      .flat_map(move |closed| {
        db::query_one::<(i64, i64)>(&db, BALANCE, params![org.as_str(), id])
          .map(move |(balance, open)| (closed, balance, open))
      })
      .map_err(JobFailure::from)
      .flat_map(move |(closed, balance, open)| {
        // Closed now, or (no longer open, nothing on it) by an earlier attempt of this activity.
        if closed > 0 || (open == 0 && balance == 0) {
          return succeed(closure);
        }
        fail(JobFailure::discard(format!(
          "account {id} holds a balance of {balance}; move it before the closure"
        )))
      })
  }
}

/// The closure engine of one Ledger instance: [`AccountClosure`] on the workflow tables of the
/// primary, its activities and timer on the Ledger's job queue. Cloning shares it.
#[derive(Clone)]
pub struct Closures {
  engine: WorkflowEngine,
}

impl Closures {
  /// The engine over `db`, reading time (leases, the grace timer) from `clock`.
  pub fn new(db: &Arc<dyn SqlClient>, clock: WallClock) -> Result<Self, WorkflowError> {
    let store = PgWorkflowStore::from_service(SqlClientService(Arc::clone(db)));
    let engine = WorkflowEngine::new(store)
      .with_clock(clock.clone())
      .with_job_queue(jobs::queue(db).with_clock(clock))
      .register(AccountClosure)?;
    Ok(Self { engine })
  }

  /// The engine (to drive instances: `run`, `resume`).
  pub fn engine(&self) -> &WorkflowEngine {
    &self.engine
  }

  /// Adds the closure activities over `db` and `storage`, and the workflow timers, to the
  /// workers' `registry`.
  pub fn register<R: HList + Send + Sync + 'static>(
    &self,
    registry: JobRegistry<R>,
    db: &Arc<dyn SqlClient>,
    storage: &ObjectStorage,
  ) -> Result<JobRegistry<R>, WorkflowError> {
    let engine = &self.engine;
    let registry = engine.register_activity(registry, Freeze(Arc::clone(db)))?;
    let registry = engine.register_activity(registry, Thaw(Arc::clone(db)))?;
    let registry = engine.register_activity(
      registry,
      ExportStatement {
        db: Arc::clone(db),
        storage: storage.clone(),
      },
    )?;
    let registry = engine.register_activity(registry, CloseAccount(Arc::clone(db)))?;
    engine.register_timers(registry)
  }

  /// Drives the instances of this engine until interrupted, starting again after a failure.
  pub fn run(&self) -> Effect<(), id_effect::Never> {
    let again = self.clone();
    self
      .engine
      .run()
      .exit::<id_effect::Never>()
      .flat_map(move |exit| {
        eprintln!("ledger: workflow engine stopped ({exit:?}); starting again");
        id_effect::sleep(Duration::from_secs(5)).flat_map(move |()| again.run())
      })
  }
}

/// A closure as `GET /accounts/{id}/closure` shows it.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct ClosureView {
  /// The workflow instance.
  pub instance: String,
  /// `running`, `completed` or `failed`.
  pub status: String,
  /// Parked: waiting for the approval, the grace period or an activity.
  pub waiting: bool,
  /// Why it failed (a rejection, a balance left on the account).
  pub error: Option<String>,
}

/// The body of `POST /accounts/{id}/closure/approval`.
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
pub struct Decision {
  /// Close the account, or keep it.
  pub approved: bool,
}

#[derive(Clone, Debug, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct InstanceRow {
  id: String,
  status: String,
  waiting: bool,
  error: Option<String>,
  requested_by: Option<String>,
}

impl InstanceRow {
  fn view(self) -> ClosureView {
    ClosureView {
      instance: self.id,
      status: self.status,
      waiting: self.waiting,
      error: self.error,
    }
  }
}

/// The latest closure of an account, read from the workflow instances by their input.
const LATEST: &str = "SELECT id, status, waiting, error, input->>'requested_by' AS requested_by \
  FROM workflow_instances \
  WHERE name = $1 AND input->>'org_id' = $2 AND input->>'account_id' = $3 \
  ORDER BY created_at DESC, id DESC LIMIT 1";

fn latest(app: &App, org: &str, account: i64) -> Effect<Option<InstanceRow>, ApiError> {
  let params = params![AccountClosure::NAME, org, account.to_string()];
  db::query_all::<InstanceRow>(&app.db, LATEST, params)
    .map(|rows| rows.into_iter().next())
    .map_err(ApiError::from)
}

fn instance_id(org: &str, account: i64, version: i64) -> Result<WorkflowId, ApiError> {
  WorkflowId::new(format!("closure:{org}:{account}:{version}")).map_err(ApiError::Workflow)
}

fn request(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<i64>,
) -> Effect<(StatusCode, Json<ClosureView>), ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let id = input.path;
  as_caller(&caps, move |caller| {
    find_account(&app, &caller, id).flat_map(move |account| {
      let allowed = caller.authorize(&app.policies, "account.manage", &account);
      let org = caller.org().to_owned();
      allowed
        .flat_map({
          let (app, org) = (Arc::clone(&app), org.clone());
          move |()| latest(&app, &org, id)
        })
        .flat_map(move |found| {
          if found.as_ref().is_some_and(|row| row.status == "running") {
            return fail(ApiError::Conflict(format!(
              "account {id} already has a closure in progress"
            )));
          }
          let instance = match instance_id(&org, id, account.version) {
            Ok(instance) => instance,
            Err(e) => return fail(e),
          };
          let closure = Closure {
            org_id: org.clone(),
            account_id: id,
            requested_by: caller.id().to_owned(),
            was_locked: account.locked,
          };
          app
            .closures
            .engine()
            .start::<AccountClosure>(instance.clone(), closure)
            .map_err(ApiError::Workflow)
            .map(move |_started| {
              let view = ClosureView {
                instance: instance.to_string(),
                status: "running".to_owned(),
                waiting: false,
                error: None,
              };
              (StatusCode::ACCEPTED, Json(view))
            })
        })
    })
  })
}

fn decide(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<i64, (), (), Decision>,
) -> Effect<(StatusCode, Json<ClosureView>), ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let (id, decision) = (input.path, input.body);
  as_caller(&caps, move |caller| {
    let org = caller.org().to_owned();
    find_account(&app, &caller, id)
      .flat_map({
        let app = Arc::clone(&app);
        move |_account| latest(&app, &org, id)
      })
      .flat_map(move |found| {
        let Some(row) = found.filter(|row| row.status == "running") else {
          return fail(ApiError::NotFound("closure"));
        };
        if row.requested_by.as_deref() == Some(caller.id()) {
          return fail(ApiError::Conflict(
            "the closure must be decided by another owner than the one who asked for it".into(),
          ));
        }
        let instance = match WorkflowId::new(row.id.clone()) {
          Ok(instance) => instance,
          Err(e) => return fail(ApiError::Workflow(e)),
        };
        let approval = Approval {
          approved: decision.approved,
          by: caller.id().to_owned(),
        };
        app
          .closures
          .engine()
          .client()
          .signal(&instance, APPROVAL, approval)
          .map_err(ApiError::Workflow)
          .map(move |_delivered| (StatusCode::ACCEPTED, Json(row.view())))
      })
  })
}

fn show(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<i64>,
) -> Effect<Json<ClosureView>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let id = input.path;
  as_caller(&caps, move |caller| {
    // A closed account is no longer found by `find_account`: its latest closure still is.
    latest(&app, caller.org(), id).flat_map(|found| match found {
      Some(row) => succeed(Json(row.view())),
      None => fail(ApiError::NotFound("closure")),
    })
  })
}

/// `/accounts/{id}/closure` routes.
pub fn routes(policies: &Policies) -> Router<AppEnv> {
  Router::new()
    .route(guarded(
      policies,
      "accounts.open",
      Route::post("/accounts/{id}/closure")
        .path::<AccountId>()
        .response::<ClosureView>(StatusCode::ACCEPTED)
        .handler_with_request_caps(request),
    ))
    .route(guarded(
      policies,
      "accounts.close",
      Route::post("/accounts/{id}/closure/approval")
        .path::<AccountId>()
        .body::<Decision>()
        .response::<ClosureView>(StatusCode::ACCEPTED)
        .handler_with_request_caps(decide),
    ))
    .route(guarded(
      policies,
      "ledger.read",
      Route::get("/accounts/{id}/closure")
        .path::<AccountId>()
        .response::<ClosureView>(StatusCode::OK)
        .handler_with_request_caps(show),
    ))
}
