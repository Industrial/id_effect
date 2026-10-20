//! Journal entries: double entry, posted in `SERIALIZABLE` transactions.
//!
//! A posting:
//!
//! 1. is decoded by [`NewEntry`]'s schema: two to 100 non-zero lines whose amounts sum to zero
//!    (debits positive, credits negative), else a `422` listing every issue;
//! 2. loads the accounts it touches (tenant-scoped, so another organisation's account is
//!    unknown) and checks `account.post` ([`can_post`](crate::policy::can_post)) on each;
//! 3. writes the entry, its postings, a hash-chained audit record and its announcement
//!    ([`EntryPosted`] in the outbox, see [`notifications`]) in one `SERIALIZABLE` transaction,
//!    rerun when it loses a serialization conflict ([`retry_serialization_failures`]);
//! 4. drops the cached balances of its accounts.
//!
//! `POST /entries/batch` posts many entries in one transaction, each under its own savepoint: an
//! entry the database refuses is rolled back alone and reported, the others commit.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use id_effect::schema::{
  self, HasSchema, ParseError, Schema, filter, length, max_items, min_items, minimum, range,
};
use id_effect::{Effect, Schedule, fail, succeed};
use id_effect_kit::authz::Policies;
use id_effect_kit::axum::RequestCaps;
use id_effect_kit::axum::route::{Input, Route, Router};
use id_effect_kit::messaging::Topic;
use id_effect_kit::sql::audit::{AuditContext, AuditEntry, AuditLog};
use id_effect_kit::sql::query_builder::Dialect;
use id_effect_kit::sql::query_builder::sea_query::{Expr, ExprTrait};
use id_effect_kit::sql::{
  BoundRepository, Entity, FromRow, Isolation, Page, Repository, SqlError, SqlTransaction,
  is_serialization_failure, retry_serialization_failures, savepoint, with_transaction_options,
};
use serde::Serialize;

use crate::accounts::{Account, accounts_of};
use crate::app::{App, AppEnv, Caller, as_caller, guarded};
use crate::error::ApiError;
use crate::notifications::{self, EntryPosted};
use crate::reports;

/// A journal entry's header.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(crate = id_effect_kit::sql, table = "journal_entries", tenant = "org_id")]
pub struct JournalEntry {
  /// Assigned by the database.
  #[entity(id, generated)]
  pub id: i64,
  /// The organisation.
  pub org_id: String,
  /// What the entry records.
  pub memo: String,
  /// Who posted it.
  pub posted_by: String,
}

/// One line of an entry, as stored.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[from_row(crate = id_effect_kit::sql)]
#[entity(crate = id_effect_kit::sql, table = "postings", tenant = "org_id")]
pub struct Posting {
  /// Assigned by the database.
  #[entity(id, generated)]
  pub id: i64,
  /// The organisation.
  pub org_id: String,
  /// The entry.
  pub entry_id: i64,
  /// The account.
  pub account_id: i64,
  /// Minor units: debit positive, credit negative.
  pub amount: i64,
}

/// One line of an entry: an amount on an account (minor units; debit positive, credit
/// negative).
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct Line {
  /// The account.
  pub account_id: i64,
  /// Never zero.
  pub amount: i64,
}

/// A posted entry: its header and lines.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct PostedEntry {
  /// The entry id.
  pub id: i64,
  /// What it records.
  pub memo: String,
  /// Who posted it.
  pub posted_by: String,
  /// Its lines, as posted.
  pub lines: Vec<Line>,
}

/// `GET /entries`.
#[derive(Clone, Debug, Serialize, id_effect::Schema)]
pub struct EntryList {
  /// Entry headers, by id.
  pub entries: Vec<JournalEntry>,
}

/// The outcome of one entry of a batch.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
pub struct BatchLine {
  /// Position in the request, from 0.
  pub index: i64,
  /// The posted entry's id.
  pub entry_id: Option<i64>,
  /// Why it was not posted.
  pub error: Option<String>,
}

/// `POST /entries/batch`.
#[derive(Clone, Debug, Serialize, id_effect::Schema)]
pub struct BatchResult {
  /// One outcome per entry, in request order.
  pub entries: Vec<BatchLine>,
}

/// The schema of one line: an account id and a non-zero amount.
pub struct NewLine;

impl HasSchema for NewLine {
  type A = Line;
  type I = (i64, i64);
  type E = ();

  fn schema() -> Schema<Line, (i64, i64)> {
    schema::transform(
      schema::struct_(
        "account_id",
        minimum(schema::i64::<()>(), 1),
        "amount",
        filter(
          schema::i64::<()>(),
          |amount: &i64| *amount != 0,
          "must not be zero".to_owned(),
        ),
      ),
      |(account_id, amount)| Ok::<_, ParseError>(Line { account_id, amount }),
      |l: Line| (l.account_id, l.amount),
    )
  }
}

/// The body of `POST /entries`: a memo and balanced lines.
#[derive(Clone, Debug, PartialEq)]
pub struct NewEntry {
  /// 1 to 200 characters.
  pub memo: String,
  /// 2 to 100 lines summing to zero.
  pub lines: Vec<Line>,
}

impl NewEntry {
  /// The distinct accounts the entry touches, ascending.
  pub fn account_ids(&self) -> Vec<i64> {
    let ids: BTreeSet<i64> = self.lines.iter().map(|l| l.account_id).collect();
    ids.into_iter().collect()
  }
}

impl HasSchema for NewEntry {
  type A = NewEntry;
  type I = (String, Vec<(i64, i64)>);
  type E = ();

  fn schema() -> Schema<NewEntry, (String, Vec<(i64, i64)>)> {
    let lines = max_items(min_items(schema::array(NewLine::schema()), 2), 100);
    schema::transform(
      schema::struct_(
        "memo",
        length(schema::string::<()>(), 1, 200),
        "lines",
        lines,
      ),
      |(memo, lines): (String, Vec<Line>)| {
        let sum: i128 = lines.iter().map(|l| i128::from(l.amount)).sum();
        if sum != 0 {
          return Err(ParseError::new(
            "lines",
            format!("debits and credits must balance (they sum to {sum})"),
          ));
        }
        Ok(NewEntry { memo, lines })
      },
      |e: NewEntry| (e.memo, e.lines),
    )
  }
}

/// The body of `POST /entries/batch`: an array of 1 to 100 entries, posted in order.
pub struct NewBatch;

impl HasSchema for NewBatch {
  type A = Vec<NewEntry>;
  type I = Vec<(String, Vec<(i64, i64)>)>;
  type E = ();

  fn schema() -> Schema<Vec<NewEntry>, Vec<(String, Vec<(i64, i64)>)>> {
    max_items(min_items(schema::array(NewEntry::schema()), 1), 100)
  }
}

/// `?limit=&offset=` of `GET /entries`.
pub struct Paging;

impl HasSchema for Paging {
  type A = Page;
  type I = (Option<i64>, Option<i64>);
  type E = ();

  fn schema() -> Schema<Page, (Option<i64>, Option<i64>)> {
    schema::transform(
      schema::struct_(
        "limit",
        schema::optional(range(schema::i64::<()>(), 1, 100)),
        "offset",
        schema::optional(minimum(schema::i64::<()>(), 0)),
      ),
      |(limit, offset): (Option<i64>, Option<i64>)| {
        let (limit, offset) = (limit.unwrap_or(50), offset.unwrap_or(0));
        Ok::<_, ParseError>(Page::new(limit.unsigned_abs(), offset.unsigned_abs()))
      },
      |p: Page| (i64::try_from(p.limit).ok(), i64::try_from(p.offset).ok()),
    )
  }
}

/// `{id}` of `GET /entries/{id}`.
pub struct EntryId;

impl HasSchema for EntryId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    minimum(schema::i64::<()>(), 1).describe("Journal entry id")
  }
}

fn repository<T: Entity>(org: &str) -> Repository<T> {
  Repository::new(Dialect::Postgres).with_tenant(org.to_owned())
}

/// How often a `SERIALIZABLE` posting that lost a conflict is rerun: up to 10 times, after an
/// exponential, jittered pause starting at 5 ms.
fn conflicts() -> Schedule<(Duration, u64), SqlError> {
  Schedule::exponential(Duration::from_millis(5))
    .jittered()
    .intersect(Schedule::recurs(10))
}

/// The accounts `ids` of the caller's organisation, all of them, or a `422` naming the unknown.
fn load_accounts(app: &App, caller: &Caller, ids: Vec<i64>) -> Effect<Vec<Account>, ApiError> {
  accounts_of(caller.org())
    .on(Arc::clone(&app.db))
    .find_by(Expr::col("id").is_in(ids.clone()))
    .map_err(ApiError::from)
    .flat_map(move |found| {
      let missing: Vec<i64> = ids
        .into_iter()
        .filter(|id| !found.iter().any(|a| a.id == *id))
        .collect();
      if missing.is_empty() {
        succeed(found)
      } else {
        fail(ApiError::invalid(
          "/lines",
          format!("unknown account(s) {missing:?}"),
        ))
      }
    })
}

/// `account.post` on every account, in order; the first denial fails.
fn authorize_all(app: &App, caller: &Caller, accounts: &[Account]) -> Effect<(), ApiError> {
  accounts.iter().fold(succeed(()), |checked, account| {
    let next = caller.authorize(&app.policies, "account.post", account);
    checked.flat_map(move |()| next)
  })
}

/// The inserts of one entry in `tx`, then its audit record and its announcement on `topic` (the
/// outbox, [`notifications`]). The accounts are checked after the inserts (still open, still
/// the caller's organisation), so a refused entry has written something its savepoint must
/// undo.
fn write_entry(
  tx: Arc<dyn SqlTransaction>,
  audit: AuditLog,
  who: AuditContext,
  topic: Topic,
  entry: NewEntry,
) -> Effect<PostedEntry, SqlError> {
  let org = who.tenant().unwrap_or_default().to_owned();
  let org_of_entry = org.clone();
  let journal: BoundRepository<JournalEntry> = repository(&org).on(Arc::clone(&tx));
  let postings: BoundRepository<Posting> = repository(&org).on(Arc::clone(&tx));
  let accounts = accounts_of(&org).on(Arc::clone(&tx));
  let ids = entry.account_ids();
  let touched = ids.clone();
  let header = JournalEntry {
    id: 0,
    org_id: org.clone(),
    memo: entry.memo.clone(),
    posted_by: who.actor().to_owned(),
  };
  journal
    .insert(header)
    .flat_map(move |header| {
      let inserts = entry.lines.iter().fold(succeed(()), |done, line| {
        let insert = postings.insert(Posting {
          id: 0,
          org_id: org.clone(),
          entry_id: header.id,
          account_id: line.account_id,
          amount: line.amount,
        });
        done.flat_map(move |()| insert.map(|_| ()))
      });
      inserts.map(move |()| PostedEntry {
        id: header.id,
        memo: header.memo,
        posted_by: header.posted_by,
        lines: entry.lines,
      })
    })
    .flat_map(move |posted| {
      let n = ids.len();
      accounts
        .count_by(Expr::col("id").is_in(ids))
        .flat_map(move |open| {
          if usize::try_from(open).ok() == Some(n) {
            succeed(posted)
          } else {
            fail(SqlError::RowNotFound)
          }
        })
    })
    .flat_map({
      let tx = Arc::clone(&tx);
      move |posted| {
        let record = AuditEntry::new("entry.post", format!("entry:{}", posted.id))
          .with_after(serde_json::to_value(&posted).unwrap_or_default());
        audit.append(&tx, &who, record).map(move |_| posted)
      }
    })
    .flat_map(move |posted| {
      let announced = EntryPosted {
        org_id: org_of_entry,
        entry_id: posted.id,
        account_ids: touched,
      };
      notifications::stage(&tx, &topic, announced).map(move |_| posted)
    })
}

/// Posts `entry` for `caller` (see the [module docs](self)).
pub fn post_entry(app: Arc<App>, caller: Caller, entry: NewEntry) -> Effect<PostedEntry, ApiError> {
  let ids = entry.account_ids();
  load_accounts(&app, &caller, ids.clone())
    .flat_map({
      let (app, caller) = (Arc::clone(&app), caller.clone());
      move |accounts| authorize_all(&app, &caller, &accounts)
    })
    .flat_map({
      let app = Arc::clone(&app);
      let who = caller.audit();
      move |()| {
        let db = Arc::clone(&app.db);
        let (audit, topic) = (app.audit.clone(), app.topic.clone());
        retry_serialization_failures(conflicts(), move || {
          let (audit, who, entry) = (audit.clone(), who.clone(), entry.clone());
          let topic = topic.clone();
          with_transaction_options(Arc::clone(&db), Isolation::Serializable, move |tx| {
            write_entry(tx, audit, who, topic, entry)
          })
        })
        .map_err(ApiError::from)
      }
    })
    .flat_map(move |posted| {
      reports::forget_balances(&app, caller.org(), &ids).map(move |()| posted)
    })
}

/// Posts every entry of `batch` in one `SERIALIZABLE` transaction, each under a savepoint.
/// Entries the caller may not post are reported without touching the database; entries the
/// database refuses are rolled back to their savepoint and reported; the rest commit.
pub fn post_batch(
  app: Arc<App>,
  caller: Caller,
  batch: Vec<NewEntry>,
) -> Effect<BatchResult, ApiError> {
  let ids: Vec<i64> = batch
    .iter()
    .flat_map(NewEntry::account_ids)
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect();
  let checks = accounts_of(caller.org())
    .on(Arc::clone(&app.db))
    .find_by(Expr::col("id").is_in(ids.clone()))
    .map_err(ApiError::from)
    .flat_map({
      let (app, caller) = (Arc::clone(&app), caller.clone());
      move |accounts| {
        // Per entry: `None` when it may be posted, else why not. Unknown accounts are left to
        // the database (the entry fails in its savepoint).
        batch.into_iter().fold(succeed(Vec::new()), |done, entry| {
          let touched: Vec<Account> = accounts
            .iter()
            .filter(|a| entry.account_ids().contains(&a.id))
            .cloned()
            .collect();
          let verdict = authorize_all(&app, &caller, &touched)
            .map(|()| None)
            .catch(|denied| succeed(Some(denied.to_string())));
          done.flat_map(move |mut all: Vec<(NewEntry, Option<String>)>| {
            verdict.map(move |verdict| {
              all.push((entry, verdict));
              all
            })
          })
        })
      }
    });
  let who = caller.audit();
  checks
    .flat_map({
      let app = Arc::clone(&app);
      move |checked| {
        let (db, audit, topic) = (Arc::clone(&app.db), app.audit.clone(), app.topic.clone());
        retry_serialization_failures(conflicts(), move || {
          let (audit, who, checked) = (audit.clone(), who.clone(), checked.clone());
          let topic = topic.clone();
          with_transaction_options(Arc::clone(&db), Isolation::Serializable, move |tx| {
            post_lines(tx, audit, who, topic, checked)
          })
        })
        .map_err(ApiError::from)
      }
    })
    .flat_map(move |entries| {
      reports::forget_balances(&app, caller.org(), &ids).map(move |()| BatchResult { entries })
    })
}

/// The entries of a batch in `tx`, each in a savepoint. A serialization failure is not an
/// entry's fault: it fails the whole transaction, which is then rerun.
fn post_lines(
  tx: Arc<dyn SqlTransaction>,
  audit: AuditLog,
  who: AuditContext,
  topic: Topic,
  checked: Vec<(NewEntry, Option<String>)>,
) -> Effect<Vec<BatchLine>, SqlError> {
  checked
    .into_iter()
    .enumerate()
    .fold(succeed(Vec::new()), |done, (index, (entry, denied))| {
      let index = i64::try_from(index).unwrap_or(i64::MAX);
      let outcome: Effect<BatchLine, SqlError> = match denied {
        Some(reason) => succeed(BatchLine {
          index,
          entry_id: None,
          error: Some(reason),
        }),
        None => {
          let (audit, who, topic) = (audit.clone(), who.clone(), topic.clone());
          savepoint(&tx, "entry", move |sp| {
            write_entry(sp, audit, who, topic, entry)
          })
          .map(move |posted| BatchLine {
            index,
            entry_id: Some(posted.id),
            error: None,
          })
          .catch(move |e| {
            if is_serialization_failure(&e) {
              fail(e)
            } else {
              succeed(BatchLine {
                index,
                entry_id: None,
                error: Some(e.to_string()),
              })
            }
          })
        }
      };
      done.flat_map(move |mut lines: Vec<BatchLine>| {
        outcome.map(move |line| {
          lines.push(line);
          lines
        })
      })
    })
}

fn post(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), (), (), NewEntry>,
) -> Effect<(StatusCode, Json<PostedEntry>), ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    post_entry(app, caller, input.body).map(|posted| (StatusCode::CREATED, Json(posted)))
  })
}

fn batch(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), (), (), Vec<NewEntry>>,
) -> Effect<Json<BatchResult>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    post_batch(app, caller, input.body).map(Json)
  })
}

fn list(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<(), Page>,
) -> Effect<Json<EntryList>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    list_entries(&app, &caller, input.query).map(Json)
  })
}

/// The entry headers of the caller's organisation in `page`, by id (`GET /entries`,
/// `Ledger.entries`).
pub fn list_entries(app: &App, caller: &Caller, page: Page) -> Effect<EntryList, ApiError> {
  repository::<JournalEntry>(caller.org())
    .on(Arc::clone(&app.db))
    .find_page(Expr::col("id").gt(0), page)
    .map(|entries| EntryList { entries })
    .map_err(ApiError::from)
}

/// The entry `id` of the caller's organisation with its lines; `404` for any other
/// (`GET /entries/{id}`, `Ledger.entry`).
pub fn find_entry(app: &App, caller: &Caller, id: i64) -> Effect<PostedEntry, ApiError> {
  let lines = repository::<Posting>(caller.org())
    .on(Arc::clone(&app.db))
    .find_by(Expr::col("entry_id").eq(id));
  repository::<JournalEntry>(caller.org())
    .on(Arc::clone(&app.db))
    .find(id)
    .zip(lines)
    .map_err(ApiError::from)
    .flat_map(|(header, lines)| match header {
      None => fail(ApiError::NotFound("journal entry")),
      Some(header) => succeed(PostedEntry {
        id: header.id,
        memo: header.memo,
        posted_by: header.posted_by,
        lines: lines
          .into_iter()
          .map(|p| Line {
            account_id: p.account_id,
            amount: p.amount,
          })
          .collect(),
      }),
    })
}

fn show(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<i64>,
) -> Effect<Json<PostedEntry>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  let id = input.path;
  as_caller(&caps, move |caller| find_entry(&app, &caller, id).map(Json))
}

/// `/entries` routes.
pub fn routes(policies: &Policies) -> Router<AppEnv> {
  Router::new()
    .route(guarded(
      policies,
      "ledger.post",
      Route::post("/entries")
        .body::<NewEntry>()
        .response::<PostedEntry>(StatusCode::CREATED)
        .handler_with_request_caps(post),
    ))
    .route(guarded(
      policies,
      "ledger.post",
      Route::post("/entries/batch")
        .body::<NewBatch>()
        .response::<BatchResult>(StatusCode::OK)
        .handler_with_request_caps(batch),
    ))
    .route(guarded(
      policies,
      "ledger.read",
      Route::get("/entries")
        .query::<Paging>()
        .response::<EntryList>(StatusCode::OK)
        .handler_with_request_caps(list),
    ))
    .route(guarded(
      policies,
      "ledger.read",
      Route::get("/entries/{id}")
        .path::<EntryId>()
        .response::<PostedEntry>(StatusCode::OK)
        .handler_with_request_caps(show),
    ))
}
