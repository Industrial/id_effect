//! Shared, kernel-agnostic fixtures for the spike handler corpus (`spikes/corpus/handlers.md`).
//!
//! Both `kernel_a` and `kernel_b` implement the same handler shapes over these exact services,
//! so annotation counts and compile errors differ only by kernel design. Every service is
//! `Clone + Send + Sync + 'static` (the bound both designs put on a capability). Async methods
//! return `Send` futures that really suspend once ([`yield_once`]), so a kernel cannot pass the
//! corpus by treating them as synchronous. Nothing here depends on an executor.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

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

/// The HTTP request stand-in handlers receive as input (the spikes have no axum).
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

/// H01: the router-level greeting (axum example 010).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Greeting(pub &'static str);

/// H04: a provider-built counter (axum example 020_capability_run_with).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counter(pub u32);

/// H08: shared mutable state behind an `Arc` (axum example 051).
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

/// H09/H10: the tenant, overridable per request (axum example 060).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tenant(pub String);

/// H10: a request-scoped capability that the base app env does *not* contain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentUser {
  pub id: String,
}

// ── effectful services ──────────────────────────────────────────────────────────────────────

/// H06: session store (axum example 040, `MemorySessionStore`).
#[derive(Clone, Debug, Default)]
pub struct Sessions(Arc<Mutex<HashMap<String, String>>>);

impl Sessions {
  pub fn put(&self, sid: &str, user_id: &str) {
    self
      .0
      .lock()
      .expect("sessions lock")
      .insert(sid.into(), user_id.into());
  }

  /// The user id for `sid`, if any.
  pub fn get(&self, sid: &str) -> impl Future<Output = Option<String>> + Send + 'static {
    let found = self.0.lock().expect("sessions lock").get(sid).cloned();
    async move {
      yield_once().await;
      found
    }
  }
}

/// Database failure (sql example 010, `SqlClient` errors).
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// H11/H12/H15/H18/H19/H20: an in-memory SQL stand-in (`TestSqlClient`).
#[derive(Clone, Debug, Default)]
pub struct Db(Arc<DbInner>);

impl Db {
  /// A db whose `table` already holds `n` rows.
  pub fn with_rows(table: &str, n: usize) -> Self {
    let db = Self::default();
    db.0
      .tables
      .lock()
      .expect("db lock")
      .insert(table.into(), (0..n).map(|i| format!("row{i}")).collect());
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
    self
      .0
      .tables
      .lock()
      .expect("db lock")
      .get(table)
      .map_or(0, Vec::len)
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
    self
      .db
      .0
      .staged
      .lock()
      .expect("db lock")
      .entry(self.id)
      .or_default()
      .push((table.into(), row.into()));
    Ok(())
  }

  pub fn commit(&self) -> impl Future<Output = Result<(), DbError>> + Send + 'static {
    let staged = self.db.0.staged.lock().expect("db lock").remove(&self.id);
    let out = self.db.check().map(|()| {
      let mut tables = self.db.0.tables.lock().expect("db lock");
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
    if self
      .db
      .0
      .staged
      .lock()
      .expect("db lock")
      .remove(&self.id)
      .is_some()
    {
      self.db.0.rollbacks.fetch_add(1, Ordering::Relaxed);
    }
  }
}

/// Missing configuration key (`id_effect_config::ConfigError` stand-in).
#[derive(Clone, Debug, PartialEq, Eq)]
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

/// H13/H14/H15/W01: a key/value config provider (`MapConfigProvider`, config examples 003/041).
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
    self.0.lock().expect("logger lock").push(line.into());
  }

  pub fn lines(&self) -> Vec<String> {
    self.0.lock().expect("logger lock").clone()
  }
}

/// Mail delivery failure.
#[derive(Clone, Debug, PartialEq, Eq)]
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
      self
        .0
        .lock()
        .expect("mailer lock")
        .push((to.into(), body.into()));
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
    self.0.lock().expect("mailer lock").clone()
  }
}

/// H18: a read-through fallback cache.
#[derive(Clone, Debug, Default)]
pub struct Cache(Arc<Mutex<HashMap<String, String>>>);

impl Cache {
  pub fn put(&self, key: &str, value: &str) {
    self
      .0
      .lock()
      .expect("cache lock")
      .insert(key.into(), value.into());
  }

  pub fn get(&self, key: &str) -> Option<String> {
    self.0.lock().expect("cache lock").get(key).cloned()
  }
}

// ── application error ───────────────────────────────────────────────────────────────────────

/// The union error of multi-service handlers (H15, H18, H19, H20, W01).
#[derive(Clone, Debug, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
  use super::*;
  use std::task::{Wake, Waker};
  use std::thread::Thread;

  struct Unpark(Thread);

  impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
      self.0.unpark();
    }
  }

  fn block_on<F: Future>(fut: F) -> F::Output {
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
      if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
        return out;
      }
      std::thread::park();
    }
  }

  fn assert_service<T: Clone + Send + Sync + 'static>() {}

  fn assert_send<T: Send + 'static>(_: &T) {}

  #[test]
  fn every_service_meets_the_capability_bound() {
    assert_service::<Greeting>();
    assert_service::<Counter>();
    assert_service::<Hits>();
    assert_service::<Tenant>();
    assert_service::<CurrentUser>();
    assert_service::<Sessions>();
    assert_service::<Db>();
    assert_service::<Tx>();
    assert_service::<ConfigSource>();
    assert_service::<Logger>();
    assert_service::<Mailer>();
    assert_service::<Cache>();
    assert_service::<Req>();
    assert_service::<AppError>();
  }

  #[test]
  fn async_methods_return_send_futures() {
    let db = Db::default();
    assert_send(&db.count("t"));
    assert_send(&db.begin());
    assert_send(&db.hang());
    assert_send(&Sessions::default().get("s"));
    assert_send(&Mailer::default().send("a@b", "x"));
  }

  #[test]
  fn yield_once_suspends_exactly_once() {
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(yield_once());
    assert!(fut.as_mut().poll(&mut cx).is_pending());
    assert!(fut.as_mut().poll(&mut cx).is_ready());
  }

  #[test]
  fn db_counts_commits_and_rolls_back() {
    let db = Db::with_rows("users", 3);
    assert_eq!(block_on(db.count("users")), Ok(3));
    let tx = block_on(db.begin()).unwrap();
    tx.insert("users", "a").unwrap();
    assert!(tx.insert("users", "boom").is_err());
    tx.rollback();
    tx.rollback();
    assert_eq!(db.rollbacks(), 1);
    let tx = block_on(db.begin()).unwrap();
    tx.insert("users", "b").unwrap();
    block_on(tx.commit()).unwrap();
    tx.rollback();
    assert_eq!(db.committed_rows("users"), 4);
    assert_eq!(db.rollbacks(), 1);
    db.set_failing(true);
    assert_eq!(
      block_on(db.count("users")),
      Err(DbError("unavailable".into()))
    );
  }

  #[test]
  fn plain_services_behave() {
    let s = Sessions::default();
    s.put("demo", "user-1");
    assert_eq!(block_on(s.get("demo")).as_deref(), Some("user-1"));
    assert_eq!(block_on(s.get("nope")), None);
    let cfg = ConfigSource::from_pairs([("REGION", "eu-west")]);
    assert_eq!(cfg.get_string("REGION").as_deref(), Ok("eu-west"));
    assert_eq!(cfg.get_string("X"), Err(ConfigError::Missing("X".into())));
    let m = Mailer::default();
    assert!(block_on(m.send("a@b", "hi")).is_ok());
    assert!(block_on(m.send("nobody", "hi")).is_err());
    assert_eq!(m.sent().len(), 1);
    let h = Hits::default();
    assert_eq!((h.bump(), h.bump(), h.get()), (1, 2, 2));
    let req = Req::new("/whoami").header("X-Tenant", "acme");
    assert_eq!(req.get_header("x-tenant"), Some("acme"));
    let e: AppError = DbError("x".into()).into();
    assert_eq!(e.to_string(), "db: x");
  }
}
