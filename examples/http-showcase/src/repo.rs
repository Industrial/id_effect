//! The todo store as a capability: the [`TodoRepo`] trait, provided as [`TodoRepoService`], with
//! a PostgreSQL implementation ([`PgTodoRepo`]) and an in-memory one ([`InMemoryTodoRepo`], used
//! without `DATABASE_URL` and by the tests).
//!
//! Every method returns an [`Effect`] that fails with [`AppError`]. Titles are unique: a
//! duplicate is [`AppError::Conflict`]. [`TodoRepo::create_many`] is all-or-nothing: on
//! PostgreSQL it runs in one transaction ([`with_transaction`]), so one conflicting title rolls
//! back every insert of the batch.
//!
//! The schema is `migrations/0001_init.sql`, applied by [`migrate`] when the service starts.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use id_effect::{Effect, fail, succeed, suspend};
use id_effect_sql::{
  SqlClient, SqlClientService, SqlError, SqlParam, SqlRow, SqlTransaction, with_transaction,
};
use serde::Serialize;

use crate::error::AppError;

/// Longest accepted title, in characters.
pub const MAX_TITLE_CHARS: usize = 200;

/// The schema migration (`migrations/0001_init.sql`), applied by [`migrate`].
pub const MIGRATION_0001: &str = include_str!("../migrations/0001_init.sql");

/// Advisory lock key held while [`migrate`] runs (one migrator at a time).
const MIGRATION_LOCK: i64 = 0x0007_0dd0_0001;

/// A stored todo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Todo {
  /// Store-assigned id.
  pub id: i64,
  /// Unique title.
  pub title: String,
  /// Whether it is done.
  pub completed: bool,
}

/// A validated title: trimmed, not empty, at most [`MAX_TITLE_CHARS`] characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Title(String);

impl Title {
  /// Validates `raw` ([`AppError::Invalid`] when it is empty or too long).
  pub fn parse(raw: &str) -> Result<Self, AppError> {
    let title = raw.trim();
    if title.is_empty() {
      return Err(AppError::Invalid("the title must not be empty".into()));
    }
    if title.chars().count() > MAX_TITLE_CHARS {
      return Err(AppError::Invalid(format!(
        "the title must be at most {MAX_TITLE_CHARS} characters"
      )));
    }
    Ok(Self(title.to_owned()))
  }

  /// The title text.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

/// A partial update: only the present fields change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TodoPatch {
  /// The new title.
  pub title: Option<Title>,
  /// The new completion state.
  pub completed: Option<bool>,
}

/// The todo store (Effect.ts: a service tag's interface).
pub trait TodoRepo: Send + Sync + 'static {
  /// Every todo, by id.
  fn list(&self) -> Effect<Vec<Todo>, AppError>;
  /// The todo `id` ([`AppError::NotFound`] when there is none).
  fn get(&self, id: i64) -> Effect<Todo, AppError>;
  /// Stores a new todo ([`AppError::Conflict`] when the title is taken).
  fn create(&self, title: Title) -> Effect<Todo, AppError>;
  /// Stores every title or none: the first conflict (with a stored title or an earlier one of
  /// the batch) fails the whole batch and nothing is stored.
  fn create_many(&self, titles: Vec<Title>) -> Effect<Vec<Todo>, AppError>;
  /// Applies `patch` to the todo `id` ([`AppError::NotFound`], [`AppError::Conflict`]).
  fn update(&self, id: i64, patch: TodoPatch) -> Effect<Todo, AppError>;
  /// Removes the todo `id` ([`AppError::NotFound`] when there is none).
  fn delete(&self, id: i64) -> Effect<(), AppError>;
}

/// Capability: the application's [`TodoRepo`] (derefs to `dyn TodoRepo`).
#[derive(Clone)]
pub struct TodoRepoService(pub Arc<dyn TodoRepo>);

impl TodoRepoService {
  /// Wraps `repo` as the capability.
  pub fn new(repo: impl TodoRepo) -> Self {
    Self(Arc::new(repo))
  }
}

impl std::ops::Deref for TodoRepoService {
  type Target = dyn TodoRepo;

  fn deref(&self) -> &Self::Target {
    &*self.0
  }
}

impl fmt::Debug for TodoRepoService {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("TodoRepoService")
      .field(&"<dyn TodoRepo>")
      .finish()
  }
}

fn ready<A: Send + 'static>(result: Result<A, AppError>) -> Effect<A, AppError> {
  match result {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

// ── in memory ───────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
struct Store {
  last_id: i64,
  todos: BTreeMap<i64, Todo>,
}

impl Store {
  fn taken(&self, title: &Title, except: Option<i64>) -> bool {
    self
      .todos
      .values()
      .any(|t| t.title == title.as_str() && Some(t.id) != except)
  }

  fn get(&self, id: i64) -> Result<Todo, AppError> {
    self.todos.get(&id).cloned().ok_or(AppError::NotFound(id))
  }

  fn insert(&mut self, title: Title) -> Result<Todo, AppError> {
    if self.taken(&title, None) {
      return Err(AppError::Conflict(title.0));
    }
    self.last_id += 1;
    let todo = Todo {
      id: self.last_id,
      title: title.0,
      completed: false,
    };
    self.todos.insert(todo.id, todo.clone());
    Ok(todo)
  }

  fn update(&mut self, id: i64, patch: TodoPatch) -> Result<Todo, AppError> {
    let mut todo = self.get(id)?;
    if let Some(title) = patch.title {
      if self.taken(&title, Some(id)) {
        return Err(AppError::Conflict(title.0));
      }
      todo.title = title.0;
    }
    if let Some(completed) = patch.completed {
      todo.completed = completed;
    }
    self.todos.insert(id, todo.clone());
    Ok(todo)
  }
}

/// A [`TodoRepo`] in process memory (lost on restart). Clones share the store.
#[derive(Clone, Debug, Default)]
pub struct InMemoryTodoRepo {
  store: Arc<Mutex<Store>>,
}

impl InMemoryTodoRepo {
  /// An empty store.
  pub fn new() -> Self {
    Self::default()
  }

  /// Runs `f` on the store when the effect runs.
  fn with<A: Send + 'static>(
    &self,
    f: impl FnOnce(&mut Store) -> Result<A, AppError> + Send + 'static,
  ) -> Effect<A, AppError> {
    let store = Arc::clone(&self.store);
    suspend(move || ready(f(&mut store.lock().unwrap_or_else(PoisonError::into_inner))))
  }
}

impl TodoRepo for InMemoryTodoRepo {
  fn list(&self) -> Effect<Vec<Todo>, AppError> {
    self.with(|s| Ok(s.todos.values().cloned().collect()))
  }

  fn get(&self, id: i64) -> Effect<Todo, AppError> {
    self.with(move |s| s.get(id))
  }

  fn create(&self, title: Title) -> Effect<Todo, AppError> {
    self.with(move |s| s.insert(title))
  }

  fn create_many(&self, titles: Vec<Title>) -> Effect<Vec<Todo>, AppError> {
    self.with(move |s| {
      // Work on a copy and keep it only when every insert succeeded.
      let mut draft = s.clone();
      let todos = titles
        .into_iter()
        .map(|title| draft.insert(title))
        .collect::<Result<Vec<_>, _>>()?;
      *s = draft;
      Ok(todos)
    })
  }

  fn update(&self, id: i64, patch: TodoPatch) -> Effect<Todo, AppError> {
    self.with(move |s| s.update(id, patch))
  }

  fn delete(&self, id: i64) -> Effect<(), AppError> {
    self.with(move |s| s.todos.remove(&id).map(drop).ok_or(AppError::NotFound(id)))
  }
}

// ── PostgreSQL ──────────────────────────────────────────────────────────────────────────────

const SELECT_ALL: &str = "SELECT id, title, completed FROM todos ORDER BY id";
const SELECT_ONE: &str = "SELECT id, title, completed FROM todos WHERE id = $1";
const INSERT: &str = "INSERT INTO todos (title) VALUES ($1) ON CONFLICT (title) DO NOTHING \
                      RETURNING id, title, completed";
const DELETE: &str = "DELETE FROM todos WHERE id = $1";

/// A [`TodoRepo`] on the [`SqlClientService`] (PostgreSQL SQL: `RETURNING`, `ON CONFLICT`),
/// normally the pool of [`PgSqlClient::layer`](id_effect_sql::pg::PgSqlClient::layer).
#[derive(Clone, Debug)]
pub struct PgTodoRepo {
  sql: SqlClientService,
}

impl PgTodoRepo {
  /// The repository over `sql` (run [`migrate`] on it first).
  pub fn new(sql: SqlClientService) -> Self {
    Self { sql }
  }
}

/// Decodes a `id, title, completed` row.
fn decode(row: &SqlRow) -> Result<Todo, AppError> {
  let bad = || AppError::Internal(format!("unexpected todo row {:?}", row.cells));
  let [id, title, completed] = row.cells.as_slice() else {
    return Err(bad());
  };
  Ok(Todo {
    id: id.parse().map_err(|_| bad())?,
    title: title.clone(),
    completed: match completed.as_str() {
      "true" => true,
      "false" => false,
      _ => return Err(bad()),
    },
  })
}

/// The single row a statement returned, or `missing` when it returned none.
fn one(rows: &[SqlRow], missing: impl FnOnce() -> AppError) -> Result<Todo, AppError> {
  rows.first().map_or_else(|| Err(missing()), decode)
}

/// Names the title in a [`AppError::Conflict`] mapped from a unique violation.
fn conflict_on(title: Option<String>) -> impl FnOnce(AppError) -> AppError + Send + 'static {
  move |err| match (err, title) {
    (AppError::Conflict(_), Some(title)) => AppError::Conflict(title),
    (err, _) => err,
  }
}

/// One `INSERT .. ON CONFLICT DO NOTHING` on `tx`: no row returned means the title is taken.
fn insert_in(tx: &Arc<dyn SqlTransaction>, title: Title) -> Effect<Todo, AppError> {
  tx.query(INSERT, &[SqlParam::Text(title.as_str().to_owned())])
    .map_err(AppError::from)
    .flat_map(move |rows| ready(one(&rows, || AppError::Conflict(title.0))))
}

impl TodoRepo for PgTodoRepo {
  fn list(&self) -> Effect<Vec<Todo>, AppError> {
    self
      .sql
      .query(SELECT_ALL, &[])
      .map_err(AppError::from)
      .flat_map(|rows| ready(rows.iter().map(decode).collect()))
  }

  fn get(&self, id: i64) -> Effect<Todo, AppError> {
    self
      .sql
      .query(SELECT_ONE, &[SqlParam::Int(id)])
      .map_err(AppError::from)
      .flat_map(move |rows| ready(one(&rows, || AppError::NotFound(id))))
  }

  fn create(&self, title: Title) -> Effect<Todo, AppError> {
    self
      .sql
      .query(INSERT, &[SqlParam::Text(title.as_str().to_owned())])
      .map_err(AppError::from)
      .flat_map(move |rows| ready(one(&rows, || AppError::Conflict(title.0))))
  }

  fn create_many(&self, titles: Vec<Title>) -> Effect<Vec<Todo>, AppError> {
    // One transaction: a conflict fails the bracket, which rolls every insert back.
    with_transaction(Arc::clone(&self.sql.0), move |tx| {
      titles.into_iter().fold(
        succeed(Vec::new()),
        move |done: Effect<Vec<Todo>, AppError>, title| {
          let tx = Arc::clone(&tx);
          done.flat_map(move |mut todos| {
            insert_in(&tx, title).map(move |todo| {
              todos.push(todo);
              todos
            })
          })
        },
      )
    })
  }

  fn update(&self, id: i64, patch: TodoPatch) -> Effect<Todo, AppError> {
    let mut params = vec![SqlParam::Int(id)];
    let mut sets = Vec::new();
    let title = patch.title.map(|t| t.0);
    if let Some(title) = &title {
      params.push(SqlParam::Text(title.clone()));
      sets.push(format!("title = ${}", params.len()));
    }
    if let Some(completed) = patch.completed {
      params.push(SqlParam::Bool(completed));
      sets.push(format!("completed = ${}", params.len()));
    }
    if sets.is_empty() {
      return self.get(id);
    }
    let sql = format!(
      "UPDATE todos SET {} WHERE id = $1 RETURNING id, title, completed",
      sets.join(", ")
    );
    self
      .sql
      .query(&sql, &params)
      .map_err(AppError::from)
      .map_err(conflict_on(title))
      .flat_map(move |rows| ready(one(&rows, || AppError::NotFound(id))))
  }

  fn delete(&self, id: i64) -> Effect<(), AppError> {
    self
      .sql
      .execute(DELETE, &[SqlParam::Int(id)])
      .map_err(AppError::from)
      .flat_map(move |n| {
        ready(if n == 0 {
          Err(AppError::NotFound(id))
        } else {
          Ok(())
        })
      })
  }
}

/// The statements of a migration script: `--` comment lines dropped, split on `;`.
fn statements(script: &str) -> Vec<String> {
  let code: Vec<&str> = script
    .lines()
    .filter(|line| !line.trim_start().starts_with("--"))
    .collect();
  code
    .join("\n")
    .split(';')
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .map(str::to_owned)
    .collect()
}

/// Applies [`MIGRATION_0001`] on `sql`, in one transaction holding an advisory lock (so
/// concurrent starts do not race on the DDL). Idempotent.
pub fn migrate(sql: Arc<dyn SqlClient>) -> Effect<(), SqlError> {
  with_transaction(sql, |tx| {
    let lock = tx
      .execute(
        "SELECT pg_advisory_xact_lock($1)",
        &[SqlParam::Int(MIGRATION_LOCK)],
      )
      .map(drop);
    statements(MIGRATION_0001)
      .into_iter()
      .fold(lock, move |done, stmt| {
        let tx = Arc::clone(&tx);
        done.flat_map(move |()| tx.execute(&stmt, &[]).map(drop))
      })
  })
}

// ── test doubles ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
pub(crate) mod doubles {
  //! Wrappers over [`InMemoryTodoRepo`] for the request-policy tests.

  use std::sync::atomic::{AtomicU32, Ordering};
  use std::sync::{Arc, Mutex, PoisonError};
  use std::time::Duration;

  use id_effect::{Effect, fail, sleep, succeed, suspend};

  use super::{InMemoryTodoRepo, Title, Todo, TodoPatch, TodoRepo};
  use crate::error::AppError;

  /// Runs `before` and then the inner repository's effect, for every method.
  macro_rules! wrap_repo {
    ($ty:ty, $before:ident) => {
      impl TodoRepo for $ty {
        fn list(&self) -> Effect<Vec<Todo>, AppError> {
          let inner = self.inner.clone();
          self.$before().flat_map(move |()| inner.list())
        }
        fn get(&self, id: i64) -> Effect<Todo, AppError> {
          let inner = self.inner.clone();
          self.$before().flat_map(move |()| inner.get(id))
        }
        fn create(&self, title: Title) -> Effect<Todo, AppError> {
          let inner = self.inner.clone();
          self.$before().flat_map(move |()| inner.create(title))
        }
        fn create_many(&self, titles: Vec<Title>) -> Effect<Vec<Todo>, AppError> {
          let inner = self.inner.clone();
          self.$before().flat_map(move |()| inner.create_many(titles))
        }
        fn update(&self, id: i64, patch: TodoPatch) -> Effect<Todo, AppError> {
          let inner = self.inner.clone();
          self.$before().flat_map(move |()| inner.update(id, patch))
        }
        fn delete(&self, id: i64) -> Effect<(), AppError> {
          let inner = self.inner.clone();
          self.$before().flat_map(move |()| inner.delete(id))
        }
      }
    };
  }

  /// Fails its first `failures` calls with [`AppError::Unavailable`], then delegates.
  #[derive(Clone, Debug)]
  pub(crate) struct FlakyRepo {
    pub(crate) inner: InMemoryTodoRepo,
    failures_left: Arc<Mutex<u32>>,
    calls: Arc<AtomicU32>,
  }

  impl FlakyRepo {
    pub(crate) fn new(inner: InMemoryTodoRepo, failures: u32) -> Self {
      Self {
        inner,
        failures_left: Arc::new(Mutex::new(failures)),
        calls: Arc::new(AtomicU32::new(0)),
      }
    }

    /// How many times a method's effect has run.
    pub(crate) fn calls(&self) -> u32 {
      self.calls.load(Ordering::SeqCst)
    }

    fn attempt(&self) -> Effect<(), AppError> {
      let (left, calls) = (Arc::clone(&self.failures_left), Arc::clone(&self.calls));
      suspend(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        let mut left = left.lock().unwrap_or_else(PoisonError::into_inner);
        if *left > 0 {
          *left -= 1;
          fail(AppError::Unavailable("flaky store".into()))
        } else {
          succeed(())
        }
      })
    }
  }

  wrap_repo!(FlakyRepo, attempt);

  /// Sleeps `delay` (on the fiber's clock) before every call.
  #[derive(Clone, Debug)]
  pub(crate) struct SlowRepo {
    pub(crate) inner: InMemoryTodoRepo,
    delay: Duration,
  }

  impl SlowRepo {
    pub(crate) fn new(inner: InMemoryTodoRepo, delay: Duration) -> Self {
      Self { inner, delay }
    }

    fn pause(&self) -> Effect<(), AppError> {
      sleep(self.delay)
    }
  }

  wrap_repo!(SlowRepo, pause);
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, AppError>) -> Result<A, AppError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn title(s: &str) -> Title {
    Title::parse(s).unwrap()
  }

  #[test]
  fn titles_are_trimmed_and_validated() {
    assert_eq!(title("  milk ").as_str(), "milk");
    assert!(matches!(Title::parse("   "), Err(AppError::Invalid(_))));
    let long = "x".repeat(MAX_TITLE_CHARS + 1);
    assert!(matches!(Title::parse(&long), Err(AppError::Invalid(_))));
    assert!(Title::parse(&"é".repeat(MAX_TITLE_CHARS)).is_ok());
  }

  #[test]
  fn in_memory_crud() {
    let repo = InMemoryTodoRepo::new();
    let a = run(repo.create(title("a"))).unwrap();
    assert_eq!(
      a,
      Todo {
        id: 1,
        title: "a".into(),
        completed: false
      }
    );
    assert_eq!(run(repo.get(1)).unwrap(), a);
    assert_eq!(run(repo.get(9)), Err(AppError::NotFound(9)));
    assert_eq!(
      run(repo.create(title("a"))),
      Err(AppError::Conflict("a".into()))
    );
    let b = run(repo.create(title("b"))).unwrap();
    let patch = TodoPatch {
      title: Some(title("a")),
      completed: None,
    };
    assert_eq!(
      run(repo.update(b.id, patch)),
      Err(AppError::Conflict("a".into()))
    );
    let patch = TodoPatch {
      title: Some(title("c")),
      completed: Some(true),
    };
    let c = run(repo.update(b.id, patch)).unwrap();
    assert_eq!((c.title.as_str(), c.completed), ("c", true));
    assert_eq!(
      run(repo.update(9, TodoPatch::default())),
      Err(AppError::NotFound(9))
    );
    assert_eq!(run(repo.list()).unwrap(), vec![a, c]);
    run(repo.delete(1)).unwrap();
    assert_eq!(run(repo.delete(1)), Err(AppError::NotFound(1)));
    assert_eq!(run(repo.list()).unwrap().len(), 1);
  }

  #[test]
  fn in_memory_batch_is_all_or_nothing() {
    let repo = InMemoryTodoRepo::new();
    run(repo.create(title("a"))).unwrap();
    assert_eq!(
      run(repo.create_many(vec![title("b"), title("a")])),
      Err(AppError::Conflict("a".into()))
    );
    assert_eq!(
      run(repo.create_many(vec![title("b"), title("b")])),
      Err(AppError::Conflict("b".into()))
    );
    assert_eq!(
      run(repo.list()).unwrap().len(),
      1,
      "nothing of either batch"
    );
    let made = run(repo.create_many(vec![title("b"), title("c")])).unwrap();
    assert_eq!(made.len(), 2);
    assert_eq!(run(repo.list()).unwrap().len(), 3);
  }

  #[test]
  fn decode_rejects_malformed_rows() {
    let row = |cells: &[&str]| SqlRow::new(cells.iter().map(|c| (*c).to_owned()).collect());
    assert_eq!(
      decode(&row(&["3", "t", "true"])).unwrap(),
      Todo {
        id: 3,
        title: "t".into(),
        completed: true
      }
    );
    assert!(!decode(&row(&["3", "t", "false"])).unwrap().completed);
    for bad in [&["x", "t", "true"][..], &["3", "t", "yes"], &["3", "t"]] {
      assert!(matches!(decode(&row(bad)), Err(AppError::Internal(_))));
    }
  }

  #[test]
  fn the_migration_splits_into_statements() {
    let stmts = statements(MIGRATION_0001);
    assert_eq!(stmts.len(), 1, "{stmts:?}");
    assert!(stmts[0].starts_with("CREATE TABLE IF NOT EXISTS todos"));
    assert_eq!(
      statements("-- only\n;\n SELECT 1; SELECT 2;"),
      ["SELECT 1", "SELECT 2"]
    );
  }

  #[test]
  fn a_conflict_from_a_unique_violation_names_the_title() {
    let named = conflict_on(Some("t".into()))(AppError::Conflict(String::new()));
    assert_eq!(named, AppError::Conflict("t".into()));
    assert_eq!(conflict_on(None)(AppError::Timeout), AppError::Timeout);
  }

  #[test]
  fn the_service_derefs_and_debugs() {
    let svc = TodoRepoService::new(InMemoryTodoRepo::new());
    assert_eq!(run(svc.list()).unwrap(), Vec::new());
    assert_eq!(format!("{svc:?}"), "TodoRepoService(\"<dyn TodoRepo>\")");
  }
}

/// Live PostgreSQL tests: they need `DATABASE_URL`. With `ID_EFFECT_REQUIRE_PG=1` (CI) a missing
/// or unreachable database **fails** them; otherwise they print `SKIP` and return.
#[cfg(test)]
mod pg_tests {
  use std::sync::atomic::{AtomicU32, Ordering};
  use std::time::{SystemTime, UNIX_EPOCH};

  use id_effect::{Cause, Exit};
  use id_effect_axum::EffectExecutor;
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};

  use super::*;

  fn pg_required() -> bool {
    std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
  }

  fn unavailable<T>(test: &str, why: &str) -> Option<T> {
    assert!(
      !pg_required(),
      "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
    );
    eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
    None
  }

  async fn exit<A: Send + 'static, E: Send + 'static>(eff: Effect<A, E>) -> Exit<A, E> {
    EffectExecutor::global().run_effect(eff).await
  }

  async fn run<A: Send + 'static>(eff: Effect<A, AppError>) -> Result<A, AppError> {
    match exit(eff).await {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  /// A migrated repository on `DATABASE_URL`, or `None` after [`unavailable`] skipped.
  async fn live_repo(test: &str) -> Option<(PgTodoRepo, SqlClientService)> {
    let url = match std::env::var("DATABASE_URL") {
      Ok(url) if !url.trim().is_empty() => url,
      _ => return unavailable(test, "DATABASE_URL is not set"),
    };
    let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(4)).await {
      Ok(pool) => pool,
      Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
    };
    let sql = SqlClientService::new(PgSqlClient::new(pool));
    if let Exit::Failure(cause) = exit(sql.connect()).await {
      return unavailable(test, &format!("cannot connect: {cause:?}"));
    }
    let migrated = exit(migrate(Arc::clone(&sql.0))).await;
    assert!(matches!(migrated, Exit::Success(())), "{migrated:?}");
    Some((PgTodoRepo::new(sql.clone()), sql))
  }

  /// A title no other test (or earlier run) uses.
  fn unique(label: &str) -> Title {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |d| d.as_nanos());
    let n = N.fetch_add(1, Ordering::SeqCst);
    Title::parse(&format!("{label}-{}-{nanos}-{n}", std::process::id())).unwrap()
  }

  async fn titles(repo: &PgTodoRepo) -> Vec<String> {
    run(repo.list())
      .await
      .unwrap()
      .into_iter()
      .map(|t| t.title)
      .collect()
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn live_postgres_crud_round_trip() {
    let Some((repo, _)) = live_repo("live_postgres_crud_round_trip").await else {
      return;
    };
    let (a, b) = (unique("crud-a"), unique("crud-b"));
    let todo = run(repo.create(a.clone())).await.unwrap();
    assert_eq!((todo.title.as_str(), todo.completed), (a.as_str(), false));
    assert_eq!(run(repo.get(todo.id)).await.unwrap(), todo);
    assert_eq!(
      run(repo.create(a.clone())).await,
      Err(AppError::Conflict(a.as_str().into()))
    );
    let other = run(repo.create(b.clone())).await.unwrap();
    let to_a = TodoPatch {
      title: Some(a.clone()),
      completed: None,
    };
    assert_eq!(
      run(repo.update(other.id, to_a)).await,
      Err(AppError::Conflict(a.as_str().into()))
    );
    let done = TodoPatch {
      title: None,
      completed: Some(true),
    };
    let updated = run(repo.update(todo.id, done)).await.unwrap();
    assert!(updated.completed);
    assert_eq!(
      run(repo.update(todo.id, TodoPatch::default()))
        .await
        .unwrap(),
      updated
    );
    assert!(titles(&repo).await.contains(&a.as_str().to_owned()));
    run(repo.delete(todo.id)).await.unwrap();
    run(repo.delete(other.id)).await.unwrap();
    assert_eq!(
      run(repo.get(todo.id)).await,
      Err(AppError::NotFound(todo.id))
    );
    assert_eq!(
      run(repo.delete(todo.id)).await,
      Err(AppError::NotFound(todo.id))
    );
    let missing = TodoPatch {
      title: None,
      completed: Some(false),
    };
    assert_eq!(
      run(repo.update(todo.id, missing)).await,
      Err(AppError::NotFound(todo.id))
    );
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn live_postgres_a_failed_batch_rolls_back() {
    let Some((repo, sql)) = live_repo("live_postgres_a_failed_batch_rolls_back").await else {
      return;
    };
    let (a, b, c) = (unique("tx-a"), unique("tx-b"), unique("tx-c"));
    let kept = run(repo.create(a.clone())).await.unwrap();
    // `b` is inserted first, then `a` conflicts: the transaction rolls `b` back.
    assert_eq!(
      run(repo.create_many(vec![b.clone(), a.clone()])).await,
      Err(AppError::Conflict(a.as_str().into()))
    );
    let now = titles(&repo).await;
    assert!(!now.contains(&b.as_str().to_owned()), "b was rolled back");
    assert!(now.contains(&a.as_str().to_owned()));
    // A conflict inside the batch rolls back too.
    assert!(matches!(
      run(repo.create_many(vec![c.clone(), c.clone()])).await,
      Err(AppError::Conflict(_))
    ));
    assert!(!titles(&repo).await.contains(&c.as_str().to_owned()));
    // A clean batch commits.
    let made = run(repo.create_many(vec![b.clone(), c.clone()]))
      .await
      .unwrap();
    let now = titles(&repo).await;
    assert!(now.contains(&b.as_str().to_owned()) && now.contains(&c.as_str().to_owned()));
    for id in made.iter().map(|t| t.id).chain([kept.id]) {
      run(repo.delete(id)).await.unwrap();
    }
    let _ = exit(sql.close()).await;
  }
}
