//! Kernel-agnostic fixtures of the handler corpus (port of spike `spikes/corpus/src/lib.rs`).
//!
//! Every service is `Clone + Send + Sync + 'static`. Async methods return `Send` futures that
//! really suspend once ([`yield_once`]), so a kernel cannot pass the corpus by treating them as
//! synchronous. Nothing here depends on an executor.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
  m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A future that returns `Pending` once (waking itself), then `Ready(())`.
pub fn yield_once() -> YieldOnce {
  YieldOnce { yielded: false }
}

/// See [`yield_once`].
#[derive(Debug)]
pub struct YieldOnce {
  yielded: bool,
}

impl Future for YieldOnce {
  type Output = ();

  fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
    if self.yielded {
      Poll::Ready(())
    } else {
      self.yielded = true;
      cx.waker().wake_by_ref();
      Poll::Pending
    }
  }
}

// ── request ─────────────────────────────────────────────────────────────────────────────────

/// The HTTP request stand-in handlers receive as input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Req {
  pub path: String,
  pub headers: Vec<(String, String)>,
}

impl Req {
  pub fn new(path: impl Into<String>) -> Self {
    Self {
      path: path.into(),
      headers: Vec::new(),
    }
  }

  pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
    self.headers.push((name.into(), value.into()));
    self
  }

  pub fn get_header(&self, name: &str) -> Option<&str> {
    self
      .headers
      .iter()
      .find(|(n, _)| n.eq_ignore_ascii_case(name))
      .map(|(_, v)| v.as_str())
  }
}

// ── plain value services ────────────────────────────────────────────────────────────────────

/// H01: the router-level greeting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Greeting(pub &'static str);

/// H04: a provider-built counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

/// H08: shared mutable state behind an `Arc`.
#[derive(Clone, Debug, Default)]
pub struct Hits(pub Arc<AtomicU32>);

impl Hits {
  /// Increment and return the new count.
  pub fn bump(&self) -> u32 {
    self.0.fetch_add(1, Ordering::Relaxed) + 1
  }

  pub fn get(&self) -> u32 {
    self.0.load(Ordering::Relaxed)
  }
}

/// H09/H10: the tenant, overridable per request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tenant(pub String);

/// H10: a request-scoped capability that the base app env does *not* contain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentUser {
  pub id: String,
}

// ── effectful services ──────────────────────────────────────────────────────────────────────

/// H06: session store.
#[derive(Clone, Debug, Default)]
pub struct Sessions(Arc<Mutex<HashMap<String, String>>>);

impl Sessions {
  pub fn put(&self, sid: &str, user_id: &str) {
    lock(&self.0).insert(sid.into(), user_id.into());
  }

  /// The user id for `sid`, if any.
  pub fn get(&self, sid: &str) -> impl Future<Output = Option<String>> + Send + 'static {
    let found = lock(&self.0).get(sid).cloned();
    async move {
      yield_once().await;
      found
    }
  }
}

/// Database failure.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DbError(pub String);

impl fmt::Display for DbError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "db: {}", self.0)
  }
}

impl std::error::Error for DbError {}

#[derive(Debug, Default)]
struct DbInner {
  tables: Mutex<HashMap<String, Vec<String>>>,
  staged: Mutex<HashMap<u64, Vec<(String, String)>>>,
  next_tx: AtomicU64,
  rollbacks: AtomicU32,
  failing: AtomicBool,
}

/// H11/H12/H15/H18/H19/H20: an in-memory SQL stand-in.
#[derive(Clone, Debug, Default)]
pub struct Db(Arc<DbInner>);

impl Db {
  /// A db whose `table` already holds `n` rows.
  pub fn with_rows(table: &str, n: usize) -> Self {
    let db = Self::default();
    lock(&db.0.tables).insert(table.into(), (0..n).map(|i| format!("row{i}")).collect());
    db
  }

  /// While set, every query and `begin` fails with `DbError("unavailable")`.
  pub fn set_failing(&self, failing: bool) {
    self.0.failing.store(failing, Ordering::Relaxed);
  }

  fn check(&self) -> Result<(), DbError> {
    if self.0.failing.load(Ordering::Relaxed) {
      Err(DbError("unavailable".into()))
    } else {
      Ok(())
    }
  }

  /// `SELECT COUNT(*) FROM table`.
  pub fn count(
    &self,
    table: &str,
  ) -> impl Future<Output = Result<usize, DbError>> + Send + 'static {
    let out = self.check().map(|()| self.committed_rows(table));
    async move {
      yield_once().await;
      out
    }
  }

  /// A query that never completes (H20's timeout path; the kernel must drop it on interrupt).
  pub fn hang(&self) -> impl Future<Output = Result<usize, DbError>> + Send + 'static {
    std::future::pending()
  }

  pub fn begin(&self) -> impl Future<Output = Result<Tx, DbError>> + Send + 'static {
    let out = self.check().map(|()| Tx {
      db: self.clone(),
      id: self.0.next_tx.fetch_add(1, Ordering::Relaxed),
    });
    async move {
      yield_once().await;
      out
    }
  }

  /// Rows committed to `table` so far.
  pub fn committed_rows(&self, table: &str) -> usize {
    lock(&self.0.tables).get(table).map_or(0, Vec::len)
  }

  /// How many transactions were rolled back (H12 asserts the finalizer ran).
  pub fn rollbacks(&self) -> u32 {
    self.0.rollbacks.load(Ordering::Relaxed)
  }
}

/// A transaction handle. `Clone` so a finalizer and the body can both hold it; `commit` and
/// `rollback` are idempotent, and `rollback` is sync so it can run from any finalizer.
#[derive(Clone, Debug)]
pub struct Tx {
  db: Db,
  id: u64,
}

impl Tx {
  /// Stage `row` into `table`. The row `"boom"` violates a constraint.
  pub fn insert(&self, table: &str, row: &str) -> Result<(), DbError> {
    if row == "boom" {
      return Err(DbError(format!("constraint violated in {table}")));
    }
    lock(&self.db.0.staged)
      .entry(self.id)
      .or_default()
      .push((table.into(), row.into()));
    Ok(())
  }

  pub fn commit(&self) -> impl Future<Output = Result<(), DbError>> + Send + 'static {
    let staged = lock(&self.db.0.staged).remove(&self.id);
    let out = self.db.check().map(|()| {
      let mut tables = lock(&self.db.0.tables);
      for (table, row) in staged.into_iter().flatten() {
        tables.entry(table).or_default().push(row);
      }
    });
    async move {
      yield_once().await;
      out
    }
  }

  /// Discard staged rows. A no-op after `commit`.
  pub fn rollback(&self) {
    if lock(&self.db.0.staged).remove(&self.id).is_some() {
      self.db.0.rollbacks.fetch_add(1, Ordering::Relaxed);
    }
  }
}

/// Missing configuration key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConfigError {
  Missing(String),
}

impl fmt::Display for ConfigError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Missing(key) => write!(f, "config: missing key {key}"),
    }
  }
}

impl std::error::Error for ConfigError {}

/// H13/H14/H15/W01: a key/value config provider.
#[derive(Clone, Debug, Default)]
pub struct ConfigSource(Arc<HashMap<String, String>>);

impl ConfigSource {
  pub fn from_pairs<const N: usize>(pairs: [(&str, &str); N]) -> Self {
    Self(Arc::new(
      pairs
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect(),
    ))
  }

  pub fn get_string(&self, key: &str) -> Result<String, ConfigError> {
    self
      .0
      .get(key)
      .cloned()
      .ok_or_else(|| ConfigError::Missing(key.into()))
  }
}

/// H10/H12/H15/H16/H17: an in-memory log sink.
#[derive(Clone, Debug, Default)]
pub struct Logger(Arc<Mutex<Vec<String>>>);

impl Logger {
  pub fn info(&self, line: impl Into<String>) {
    lock(&self.0).push(line.into());
  }

  pub fn lines(&self) -> Vec<String> {
    lock(&self.0).clone()
  }
}

/// Mail delivery failure.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MailError(pub String);

impl fmt::Display for MailError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "mail: {}", self.0)
  }
}

impl std::error::Error for MailError {}

/// H17/H19: outbound mail. Sending to an address without `@` fails.
#[derive(Clone, Debug, Default)]
pub struct Mailer(Arc<Mutex<Vec<(String, String)>>>);

impl Mailer {
  pub fn send(
    &self,
    to: &str,
    body: &str,
  ) -> impl Future<Output = Result<(), MailError>> + Send + 'static {
    let out = if to.contains('@') {
      lock(&self.0).push((to.into(), body.into()));
      Ok(())
    } else {
      Err(MailError(format!("bad address {to}")))
    };
    async move {
      yield_once().await;
      out
    }
  }

  pub fn sent(&self) -> Vec<(String, String)> {
    lock(&self.0).clone()
  }
}

/// H18: a read-through fallback cache.
#[derive(Clone, Debug, Default)]
pub struct Cache(Arc<Mutex<HashMap<String, String>>>);

impl Cache {
  pub fn put(&self, key: &str, value: &str) {
    lock(&self.0).insert(key.into(), value.into());
  }

  pub fn get(&self, key: &str) -> Option<String> {
    lock(&self.0).get(key).cloned()
  }
}

// ── application error ───────────────────────────────────────────────────────────────────────

/// The union error of multi-service handlers (H15, H18, H19, H20, W01).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AppError {
  Db(DbError),
  Config(ConfigError),
  Mail(MailError),
  Timeout,
  Unauthorized,
}

impl fmt::Display for AppError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Db(e) => e.fmt(f),
      Self::Config(e) => e.fmt(f),
      Self::Mail(e) => e.fmt(f),
      Self::Timeout => f.write_str("timed out"),
      Self::Unauthorized => f.write_str("unauthorized"),
    }
  }
}

impl std::error::Error for AppError {}

impl From<DbError> for AppError {
  fn from(e: DbError) -> Self {
    Self::Db(e)
  }
}

impl From<ConfigError> for AppError {
  fn from(e: ConfigError) -> Self {
    Self::Config(e)
  }
}

impl From<MailError> for AppError {
  fn from(e: MailError) -> Self {
    Self::Mail(e)
  }
}
