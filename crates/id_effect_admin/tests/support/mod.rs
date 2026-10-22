//! Shared fixtures of the admin editing tests: an SQLite database whose audit statements are
//! answered in memory (the audit log's SQL is PostgreSQL), the models, and a browser session
//! behind `SecurityLayer` (CSRF cookie and masked token).

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use id_effect::schema::{self, HasSchema, Schema};
use id_effect::{Effect, Exit, succeed};
use id_effect_admin::testing::{AdminRequest, AdminResponse};
use id_effect_admin::{AdminDatabase, AdminPermission, AdminSite, Guarded};
use id_effect_authz::{BasicPrincipal, Guard, Rbac, require_role};
use id_effect_axum::EffectExecutor;
use id_effect_axum::server::security::{CookieKey, SecurityLayer};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{
  Dialect, Entity, FromRow, SqlClient, SqlError, SqlRow, SqlTransaction, SqlValue,
};

// ---- the models ----

/// An article: editable title, pages, flag, note and author; a secret the forms never bind; a
/// version for optimistic locking.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(
  table = "ed_articles",
  tenant = "org_id",
  belongs_to(Author, foreign_key = "author_id", name = "author"),
  many_to_many(
    Tag,
    through = "ed_article_tags",
    local_key = "article_id",
    foreign_key = "tag_id",
    name = "tags"
  )
)]
pub struct Article {
  #[entity(id, generated)]
  pub id: i64,
  #[entity(editable)]
  pub title: String,
  #[entity(editable)]
  pub pages: i64,
  #[entity(editable)]
  pub published: bool,
  #[entity(editable)]
  pub note: Option<String>,
  #[entity(editable)]
  pub author_id: Option<i64>,
  pub secret: String,
  #[entity(version)]
  pub version: i64,
}

/// An author: the id is typed in on creation; the articles are an inline list.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(
  table = "ed_authors",
  tenant = "org_id",
  has_many(Article, foreign_key = "author_id", name = "articles")
)]
pub struct Author {
  #[entity(id)]
  pub id: i64,
  #[entity(editable)]
  pub name: String,
}

/// A tag, paired with articles in a join table; its tenant column is a field, its colour optional.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "ed_tags", tenant = "org_id")]
pub struct Tag {
  #[entity(id)]
  pub id: i64,
  pub org_id: i64,
  #[entity(editable)]
  pub name: String,
  #[entity(editable)]
  pub color: Option<String>,
}

/// A note decoded through its `Schema` (`#[from_row(schema)]`): the body must not be empty.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[from_row(schema)]
#[entity(table = "ed_notes")]
pub struct Note {
  #[entity(id, generated)]
  pub id: i64,
  #[entity(editable)]
  pub body: String,
}

impl HasSchema for Note {
  type A = Note;
  type I = (i64, String);
  type E = ();

  fn schema() -> Schema<Note, (i64, String), ()> {
    schema::transform(
      schema::struct_(
        "id",
        schema::i64(),
        "body",
        schema::validators::non_empty(schema::string()),
      ),
      |(id, body)| Ok(Note { id, body }),
      |n: Note| (n.id, n.body),
    )
  }
}

// ---- the database ----

pub async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> A {
  match EffectExecutor::global().run_effect(eff).await {
    Exit::Success(a) => a,
    Exit::Failure(cause) => panic!("{cause:?}"),
  }
}

/// The audit rows appended (committed) and the chain heads.
#[derive(Default)]
struct AuditState {
  rows: Vec<SqlRow>,
  heads: HashMap<String, (i64, Vec<u8>)>,
}

/// An SQLite client that answers the audit log's three statements in memory and forwards the
/// rest; audit rows commit and roll back with their transaction.
#[derive(Clone)]
pub struct Db {
  inner: Arc<dyn SqlClient>,
  audit: Arc<Mutex<AuditState>>,
}

impl Db {
  /// The committed audit rows (`audit_log` columns), in order.
  pub fn audit_rows(&self) -> Vec<SqlRow> {
    self.audit.lock().unwrap().rows.clone()
  }

  pub async fn exec(&self, sql: &str) {
    run(self.inner.execute(sql, &[])).await;
  }

  pub async fn rows(&self, sql: &str) -> Vec<SqlRow> {
    run(self.inner.query(sql, &[])).await
  }

  pub fn client(&self) -> Arc<dyn SqlClient> {
    Arc::new(self.clone())
  }
}

impl SqlClient for Db {
  fn connect(&self) -> Effect<(), SqlError> {
    self.inner.connect()
  }

  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.inner.query(sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    self.inner.execute(sql, params)
  }

  fn begin(&self) -> Effect<Arc<dyn SqlTransaction>, SqlError> {
    let audit = Arc::clone(&self.audit);
    self.inner.begin().map(move |inner| {
      Arc::new(Tx {
        inner,
        audit,
        pending: Arc::default(),
      }) as Arc<dyn SqlTransaction>
    })
  }
}

struct Tx {
  inner: Arc<dyn SqlTransaction>,
  audit: Arc<Mutex<AuditState>>,
  pending: Arc<Mutex<AuditState>>,
}

fn text(v: &SqlValue) -> String {
  match v {
    SqlValue::Text(t) => t.clone(),
    other => panic!("expected text, got {other:?}"),
  }
}

fn bytes(v: &SqlValue) -> Vec<u8> {
  match v {
    SqlValue::Bytes(b) => b.clone(),
    other => panic!("expected bytes, got {other:?}"),
  }
}

impl SqlTransaction for Tx {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    if sql.starts_with("INSERT INTO audit_log_head") {
      let chain = text(&params[0]);
      let pending = self.pending.lock().unwrap();
      let committed = self.audit.lock().unwrap();
      let (seq, hash) = pending
        .heads
        .get(&chain)
        .or_else(|| committed.heads.get(&chain))
        .cloned()
        .unwrap_or((0, vec![0; 32]));
      let at = jiff::Timestamp::from_microsecond(jiff::Timestamp::now().as_microsecond()).unwrap();
      return succeed(vec![SqlRow::from_pairs([
        ("seq", SqlValue::I64(seq)),
        ("hash", SqlValue::Bytes(hash)),
        ("at", SqlValue::Timestamp(at)),
      ])]);
    }
    if sql.starts_with("INSERT INTO audit_log ") {
      let mut pending = self.pending.lock().unwrap();
      let committed = self.audit.lock().unwrap().rows.len();
      let id = (committed + pending.rows.len() + 1) as i64;
      let names = [
        "chain",
        "seq",
        "actor",
        "tenant",
        "action",
        "entity",
        "before",
        "after",
        "at",
        "prev_hash",
        "hash",
      ];
      let mut pairs: Vec<(&str, SqlValue)> = vec![("id", SqlValue::I64(id))];
      pairs.extend(names.iter().copied().zip(params.iter().cloned()));
      pending.rows.push(SqlRow::from_pairs(pairs));
      return succeed(vec![SqlRow::from_pairs([("id", SqlValue::I64(id))])]);
    }
    self.inner.query(sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    if sql.starts_with("UPDATE audit_log_head") {
      let chain = text(&params[0]);
      let SqlValue::I64(seq) = params[1] else {
        panic!("seq")
      };
      self
        .pending
        .lock()
        .unwrap()
        .heads
        .insert(chain, (seq, bytes(&params[2])));
      return succeed(1);
    }
    self.inner.execute(sql, params)
  }

  // The effects are built before they run (a transaction's commit is built with it): move the
  // pending rows when the commit has run, not when it is built.
  fn commit(&self) -> Effect<(), SqlError> {
    let (pending, audit) = (Arc::clone(&self.pending), Arc::clone(&self.audit));
    self.inner.commit().map(move |()| {
      let pending = std::mem::take(&mut *pending.lock().unwrap());
      let mut audit = audit.lock().unwrap();
      audit.rows.extend(pending.rows);
      audit.heads.extend(pending.heads);
    })
  }

  fn rollback(&self) -> Effect<(), SqlError> {
    let pending = Arc::clone(&self.pending);
    self.inner.rollback().map(move |()| {
      *pending.lock().unwrap() = AuditState::default();
    })
  }
}

/// Org 1: authors 1 (Ada), 2 (Brian); tags 1 (rust), 2 (sql), 3 (web), 4 (unnamed); articles 1..=3 (article 1
/// by Ada, tagged rust). Org 2: author 9 (Globex), tag 9 (secret), article 9.
pub async fn seeded() -> Db {
  let pool = sqlite_pool_from_config(SqlitePoolConfig::memory().with_max_size(4))
    .await
    .expect("sqlite pool");
  let db = Db {
    inner: Arc::new(SqliteSqlClient::new(pool)),
    audit: Arc::default(),
  };
  for sql in [
    "CREATE TABLE ed_authors (id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL)",
    "CREATE TABLE ed_tags (id INTEGER PRIMARY KEY, org_id INTEGER NOT NULL, name TEXT NOT NULL, \
     color TEXT)",
    "CREATE TABLE ed_articles (id INTEGER PRIMARY KEY AUTOINCREMENT, org_id INTEGER NOT NULL, \
     title TEXT NOT NULL, pages INTEGER NOT NULL, published BOOLEAN NOT NULL, note TEXT, \
     author_id INTEGER, secret TEXT NOT NULL, version INTEGER NOT NULL)",
    "CREATE TABLE ed_article_tags (article_id INTEGER NOT NULL, tag_id INTEGER NOT NULL, \
     PRIMARY KEY (article_id, tag_id))",
    "CREATE TABLE ed_notes (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL)",
    "INSERT INTO ed_authors (id, org_id, name) VALUES (1, 1, 'Ada'), (2, 1, 'Brian'), \
     (9, 2, 'Globex')",
    "INSERT INTO ed_tags (id, org_id, name) VALUES (1, 1, 'rust'), (2, 1, 'sql'), (3, 1, 'web'), \
     (4, 1, ''), (9, 2, 'secret')",
    "INSERT INTO ed_articles (id, org_id, title, pages, published, note, author_id, secret, \
     version) VALUES (1, 1, 'First', 10, 1, NULL, 1, 'hunter2', 1), \
     (2, 1, 'Second', 20, 0, 'n', NULL, 'hunter2', 1), \
     (3, 1, 'Third', 30, 0, NULL, 2, 'hunter2', 1), \
     (9, 2, 'Globex plan', 1, 0, NULL, 9, 'x', 1)",
    "INSERT INTO ed_article_tags (article_id, tag_id) VALUES (1, 1), (9, 9)",
  ] {
    db.exec(sql).await;
  }
  db
}

// ---- the site ----

pub fn perms(entity: &str) -> Vec<String> {
  AdminPermission::all_for(entity)
    .unwrap()
    .iter()
    .map(ToString::to_string)
    .collect()
}

/// `editor` holds every permission on every entity; `viewer` only list and view.
pub fn rbac() -> Rbac {
  let mut builder = Rbac::builder();
  for entity in ["article", "author", "tag", "note"] {
    builder = builder.grant("editor", perms(entity)).grant(
      "viewer",
      [
        format!("admin.{entity}.list"),
        format!("admin.{entity}.view"),
      ],
    );
  }
  builder.build()
}

pub fn guarded(site: AdminSite) -> AdminSite<Guarded> {
  site
    .rbac(rbac())
    .guard(Guard::new("staff", require_role("staff")))
}

fn key() -> CookieKey {
  CookieKey::from_bytes(&[7; 64]).expect("64-byte key")
}

/// The admin behind the CSRF `SecurityLayer`.
pub fn app(site: AdminSite<Guarded>, db: &Db) -> axum::Router {
  site
    .router::<()>(AdminDatabase::new(db.client(), Dialect::Sqlite))
    .into_axum()
    .layer(SecurityLayer::recommended(key()))
}

/// The admin without the `SecurityLayer` (no CSRF check ran).
pub fn bare_app(site: AdminSite<Guarded>, db: &Db) -> axum::Router {
  site
    .router::<()>(AdminDatabase::new(db.client(), Dialect::Sqlite))
    .into_axum()
}

pub fn editor() -> BasicPrincipal {
  BasicPrincipal::new("ed")
    .with_role("staff")
    .with_role("editor")
}

pub fn viewer() -> BasicPrincipal {
  BasicPrincipal::new("vi")
    .with_role("staff")
    .with_role("viewer")
}

/// A browser session of `principal` in org 1: the CSRF cookie and a masked token, read from the
/// form page `form`.
#[derive(Clone, Debug)]
pub struct Session {
  pub principal: BasicPrincipal,
  pub cookie: String,
  pub token: String,
}

impl Session {
  pub async fn open(app: &axum::Router, principal: BasicPrincipal, form: &str) -> Self {
    let page = AdminRequest::get(form)
      .principal(principal.clone())
      .tenant(1i64)
      .send(app)
      .await;
    assert_eq!(page.status, 200, "{}", page.body);
    Self {
      principal,
      cookie: page.csrf_cookie().expect("csrf cookie"),
      token: page.csrf_token().expect("csrf token in the form"),
    }
  }

  /// `GET uri` in org 1.
  pub async fn get(&self, app: &axum::Router, uri: &str) -> AdminResponse {
    AdminRequest::get(uri)
      .principal(self.principal.clone())
      .tenant(1i64)
      .header(
        "cookie",
        axum::http::HeaderValue::from_str(&self.cookie).unwrap(),
      )
      .send(app)
      .await
  }

  /// `POST uri` with the form `body`, the cookie and the token, in org 1.
  pub async fn post(&self, app: &axum::Router, uri: &str, body: &str) -> AdminResponse {
    AdminRequest::post(uri, body)
      .principal(self.principal.clone())
      .tenant(1i64)
      .csrf(&self.cookie, &self.token)
      .send(app)
      .await
  }
}

/// `pairs` urlencoded.
pub fn form(pairs: &[(&str, &str)]) -> String {
  pairs
    .iter()
    .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
    .collect::<Vec<_>>()
    .join("&")
}

fn encode(text: &str) -> String {
  text
    .bytes()
    .map(|b| {
      if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
        char::from(b).to_string()
      } else {
        format!("%{b:02X}")
      }
    })
    .collect()
}

pub async fn article(db: &Db, id: i64) -> Option<SqlRow> {
  db.rows(&format!(
    "SELECT id, title, pages, published, note, author_id, secret, version FROM ed_articles \
     WHERE id = {id}"
  ))
  .await
  .into_iter()
  .next()
}
