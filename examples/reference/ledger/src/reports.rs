//! Reads that may lag: balances (cached, `ETag`ged), the trial balance, the CSV export of the
//! postings and the audit-chain check.
//!
//! Balances and reports read through [`App::reads`]: a `RoutingSqlClient` sending them to a read
//! replica when there is one. A balance is cached (Redis when configured, so every instance
//! shares it) until a posting on its account drops it.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Bytes;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use id_effect::{Cache, Effect, Stream, die, fail, succeed};
use id_effect_kit::authz::Policies;
use id_effect_kit::axum::RequestCaps;
use id_effect_kit::axum::body::stream_body;
use id_effect_kit::axum::middleware::HttpCacheLayer;
use id_effect_kit::axum::route::{Input, Route, Router};
use id_effect_kit::redis::{RedisCacheBackend, RedisClient};
use id_effect_kit::sql::{FromRow, SqlClient, SqlError, params};
use serde::{Deserialize, Serialize};

use crate::accounts::{AccountId, find_account};
use crate::app::{App, AppEnv, Caller, as_caller, guarded};
use crate::db;
use crate::error::ApiError;

/// An account's balance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, id_effect::Schema)]
pub struct Balance {
  /// The account.
  pub account_id: i64,
  /// Sum of its postings (minor units).
  pub balance: i64,
  /// How many postings it has.
  pub postings: i64,
}

/// What a cached balance is stored under: `<org>:<account>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BalanceKey {
  /// The organisation.
  pub org: String,
  /// The account.
  pub account: i64,
}

impl fmt::Display for BalanceKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}:{}", self.org, self.account)
  }
}

/// Balances by account, filled from the read replicas.
pub type BalanceCache = Cache<BalanceKey, Balance, SqlError>;

/// The Redis key prefix of cached balances (`ledger:balance:<org>:<account>`).
pub const BALANCE_NAMESPACE: &str = "ledger:balance";

/// The first line of `GET /export/postings.csv`.
pub const CSV_HEADER: &str = "entry_id,account,amount,memo,posted_by";

/// How long a balance stays cached when no posting drops it first.
pub const BALANCE_TTL: Duration = Duration::from_secs(300);

/// The balance cache over `reads`: in Redis when there is a client, else in this process. Only
/// balances are stored, never a failed lookup.
pub fn balance_cache(reads: Arc<dyn SqlClient>, redis: Option<RedisClient>) -> BalanceCache {
  let lookup = move |key: BalanceKey| read_balance(&reads, &key);
  match redis {
    Some(redis) => Cache::with_backend(
      RedisCacheBackend::new(redis, BALANCE_NAMESPACE, Some(BALANCE_TTL)),
      lookup,
    ),
    None => {
      let in_process: Cache<BalanceKey, Balance> =
        Cache::new(10_000, Some(BALANCE_TTL), |_: BalanceKey| {
          die("the in-process balance store is only written to")
        });
      Cache::with_backend(in_process, lookup)
    }
  }
}

fn read_balance(reads: &Arc<dyn SqlClient>, key: &BalanceKey) -> Effect<Balance, SqlError> {
  let sql = "SELECT COALESCE(SUM(amount), 0)::int8 AS balance, COUNT(*)::int8 AS postings \
             FROM postings WHERE org_id = $1 AND account_id = $2";
  let account_id = key.account;
  db::query_one::<(i64, i64)>(reads, sql, params![key.org.as_str(), account_id]).map(
    move |(balance, postings)| Balance {
      account_id,
      balance,
      postings,
    },
  )
}

/// Drops the cached balances of `accounts` (after a posting on them).
pub fn forget_balances(app: &App, org: &str, accounts: &[i64]) -> Effect<(), ApiError> {
  accounts.iter().fold(succeed(()), |done, &account| {
    let key = BalanceKey {
      org: org.to_owned(),
      account,
    };
    let forget = app.balances.invalidate::<ApiError, id_effect::HNil>(&key);
    done.flat_map(move |()| forget)
  })
}

/// One row of the trial balance.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
pub struct TrialBalanceRow {
  /// The account.
  pub account_id: i64,
  /// Its code.
  pub code: String,
  /// Its name.
  pub name: String,
  /// Its kind.
  pub kind: String,
  /// Its balance.
  pub balance: i64,
}

/// `GET /reports/trial-balance`: every open account's balance; they sum to zero.
#[derive(Clone, Debug, Serialize, id_effect::Schema)]
pub struct TrialBalance {
  /// By account code.
  pub accounts: Vec<TrialBalanceRow>,
  /// The sum of every balance (zero in a consistent ledger).
  pub total: i64,
}

/// `GET /audit/verify`: the state of the organisation's audit chain.
#[derive(Clone, Debug, Serialize, id_effect::Schema)]
pub struct AuditReport {
  /// No record was edited, removed or relinked.
  pub intact: bool,
  /// Records verified intact from the start of the chain.
  pub records: i64,
  /// Hash (hex) of the last intact record; anchor it outside the database.
  pub head: Option<String>,
  /// The first broken link, when there is one.
  pub broken: Option<String>,
}

fn balance(
  env: &mut AppEnv,
  caps: RequestCaps,
  input: Input<i64>,
) -> Effect<Json<Balance>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    account_balance(app, &caller, input.path).map(Json)
  })
}

/// The balance of the caller's account `id`, from the cache (`GET /accounts/{id}/balance`,
/// `Ledger.balance`); `404` for an account of another organisation.
pub fn account_balance(app: Arc<App>, caller: &Caller, id: i64) -> Effect<Balance, ApiError> {
  let key = BalanceKey {
    org: caller.org().to_owned(),
    account: id,
  };
  find_account(&app, caller, id)
    .flat_map(move |_account| app.balances.get(key).map_err(ApiError::from))
}

/// One posting of an account's statement ([`statement_page`]), with the balance after it.
#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
pub struct StatementLine {
  /// The posting (its position in the statement: ascending).
  pub posting_id: i64,
  /// When its entry was posted, `YYYY-MM-DD` (UTC).
  pub date: String,
  /// The journal entry.
  pub entry_id: i64,
  /// The entry's memo.
  pub memo: String,
  /// Minor units, debit positive.
  pub amount: i64,
  /// The account's balance after this posting.
  pub balance: i64,
}

/// How many postings one statement read returns.
pub const STATEMENT_PAGE: i64 = 200;

/// The postings of the caller's account `account` after the posting `after` (`0`: from the
/// first), at most [`STATEMENT_PAGE`], each with the running balance (`Ledger.statement` streams
/// them page by page). Read on the primary, so a statement includes what was just posted.
pub fn statement_page(
  app: &App,
  caller: &Caller,
  account: i64,
  after: i64,
) -> Effect<Vec<StatementLine>, ApiError> {
  let sql = "SELECT posting_id, date, entry_id, memo, amount, balance FROM ( \
               SELECT p.id AS posting_id, \
                 to_char(e.posted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS date, \
                 e.id AS entry_id, e.memo, p.amount, \
                 (SUM(p.amount) OVER (ORDER BY p.id))::int8 AS balance \
               FROM postings p JOIN journal_entries e ON e.id = p.entry_id \
               WHERE p.org_id = $1 AND p.account_id = $2) lines \
             WHERE posting_id > $3 ORDER BY posting_id LIMIT $4";
  db::query_all::<StatementLine>(
    &app.db,
    sql,
    params![caller.org(), account, after, STATEMENT_PAGE],
  )
  .map_err(ApiError::from)
}

fn trial_balance(
  env: &mut AppEnv,
  caps: RequestCaps,
  _: Input,
) -> Effect<Json<TrialBalance>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    let sql = "SELECT a.id AS account_id, a.code, a.name, a.kind, \
               COALESCE(SUM(p.amount), 0)::int8 AS balance \
               FROM accounts a LEFT JOIN postings p ON p.account_id = a.id AND p.org_id = a.org_id \
               WHERE a.org_id = $1 AND a.closed_at IS NULL \
               GROUP BY a.id ORDER BY a.code";
    db::query_all::<TrialBalanceRow>(&app.reads, sql, params![caller.org()])
      .map(|accounts| {
        let total = accounts.iter().map(|a| a.balance).sum();
        Json(TrialBalance { accounts, total })
      })
      .map_err(ApiError::from)
  })
}

/// One posting of the CSV export.
#[derive(FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct ExportRow {
  entry_id: i64,
  account_code: String,
  amount: i64,
  memo: String,
  posted_by: String,
}

impl ExportRow {
  fn csv(&self) -> Bytes {
    Bytes::from(format!(
      "{},{},{},{},{}\n",
      self.entry_id,
      csv_field(&self.account_code),
      self.amount,
      csv_field(&self.memo),
      csv_field(&self.posted_by)
    ))
  }
}

/// RFC 4180 quoting when a field needs it.
fn csv_field(raw: &str) -> String {
  if raw.contains([',', '"', '\n', '\r']) {
    format!("\"{}\"", raw.replace('"', "\"\""))
  } else {
    raw.to_owned()
  }
}

/// `GET /export/postings.csv`: every posting, streamed from the database row by row (a slow
/// client slows the query down instead of filling memory; a client that goes away stops it).
fn export(env: &mut AppEnv, caps: RequestCaps, _: Input) -> Effect<Response, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    let sql = "SELECT p.entry_id, a.code AS account_code, p.amount, e.memo, e.posted_by \
               FROM postings p JOIN accounts a ON a.id = p.account_id \
               JOIN journal_entries e ON e.id = p.entry_id \
               WHERE p.org_id = $1 ORDER BY p.entry_id, p.id";
    let header = Stream::from_iterable([Bytes::from(format!("{CSV_HEADER}\n"))]);
    let rows = app
      .reads
      .query_stream(sql, &params![caller.org()])
      .map_effect(|row| match ExportRow::from_row(&row) {
        Ok(row) => succeed(row.csv()),
        Err(e) => fail(e),
      });
    let csv = [(header::CONTENT_TYPE, "text/csv; charset=utf-8")];
    succeed((csv, stream_body(header.chain(rows))).into_response())
  })
}

fn verify_audit(
  env: &mut AppEnv,
  caps: RequestCaps,
  _: Input,
) -> Effect<Json<AuditReport>, ApiError, AppEnv> {
  let app = Arc::clone(&env.0);
  as_caller(&caps, move |caller| {
    app
      .audit
      .verify_chain_on(Arc::clone(&app.db), Some(caller.org()))
      .map(|report| {
        Json(AuditReport {
          intact: report.is_intact(),
          records: i64::try_from(report.records).unwrap_or(i64::MAX),
          head: report.head,
          broken: report.first_broken.map(|link| link.to_string()),
        })
      })
      .map_err(ApiError::from)
  })
}

/// Reports and the audit check.
pub fn routes(policies: &Policies) -> Router<AppEnv> {
  Router::new()
    .route(guarded(
      policies,
      "ledger.read",
      Route::get("/accounts/{id}/balance")
        .path::<AccountId>()
        .response::<Balance>(StatusCode::OK)
        .handler_with_request_caps(balance)
        // An ETag on every balance: a client revalidating an unchanged one gets `304`. Weak,
        // because compression sits outside it.
        .layer(HttpCacheLayer::new().weak_etags()),
    ))
    .route(guarded(
      policies,
      "ledger.read",
      Route::get("/reports/trial-balance")
        .response::<TrialBalance>(StatusCode::OK)
        .handler_with_request_caps(trial_balance),
    ))
    .route(guarded(
      policies,
      "ledger.read",
      Route::get("/export/postings.csv").handler_with_request_caps(export),
    ))
    .route(guarded(
      policies,
      "audit.verify",
      Route::get("/audit/verify")
        .response::<AuditReport>(StatusCode::OK)
        .handler_with_request_caps(verify_audit),
    ))
}

#[cfg(test)]
mod tests {
  use super::{BalanceKey, csv_field};

  #[test]
  fn csv_fields_are_quoted_when_needed() {
    assert_eq!(csv_field("rent"), "rent");
    assert_eq!(csv_field("rent, march"), "\"rent, march\"");
    assert_eq!(csv_field("the \"big\" one"), "\"the \"\"big\"\" one\"");
  }

  #[test]
  fn balance_keys_name_org_and_account() {
    let key = BalanceKey {
      org: "acme".into(),
      account: 7,
    };
    assert_eq!(key.to_string(), "acme:7");
  }
}
