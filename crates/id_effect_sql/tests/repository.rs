//! `#[derive(Entity)]` and `Repository<T>` (feature `repository`): the statements each dialect
//! gets (over `TestSqlClient`, MySQL included), then the same suite on every live backend —
//! SQLite in memory (feature `sqlite`) and PostgreSQL at `DATABASE_URL` (feature `pg`; without
//! it the tests print `SKIP`, and `ID_EFFECT_REQUIRE_PG=1` turns that into a failure).
//!
//! data.orm #1 (entity mapping), #5 (`stale_version_is_rejected`), #8
//! (`global_filter_hides_soft_deleted`, `tenant_filter_scopes_every_query`).

use std::sync::Arc;

use id_effect::{Cause, Effect, Exit, fail, hlist};
use id_effect_sql::query_builder::sea_query::{Condition, Expr, ExprTrait};
use id_effect_sql::{
  CurrentTenant, Dialect, Entity, FromRow, Page, Repository, SqlClient, SqlClientEnv,
  SqlClientService, SqlError, SqlRow, SqlTransaction, SqlTransactionService, SqlValue,
  TestSqlClient, with_transaction,
};

/// Optimistic locking (data.orm #5).
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "repo_ledgers")]
struct Ledger {
  #[entity(id)]
  id: i64,
  title: String,
  #[entity(version)]
  version: i64,
}

fn not_archived() -> Expr {
  Expr::col("archived").eq(false)
}

/// Soft delete plus a custom scope (data.orm #8).
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "repo_notes", soft_delete = "deleted_at", scope = not_archived)]
struct Note {
  #[entity(id)]
  id: i64,
  #[from_row(rename = "title_text")]
  title: String,
  archived: bool,
}

/// Tenant scoping (data.orm #8, eco.tenancy #3, sec.authz #5).
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "repo_invoices", tenant = "org_id")]
struct Invoice {
  #[entity(id)]
  id: i64,
  org_id: i64,
  amount: i64,
}

/// A database-assigned key, CRUD and paging (data.orm #1, #2).
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
struct RepoItem {
  #[entity(id, generated)]
  id: i64,
  name: String,
  qty: i64,
}

fn note(id: i64, title: &str, archived: bool) -> Note {
  Note {
    id,
    title: title.into(),
    archived,
  }
}

fn invoice(id: i64, org_id: i64, amount: i64) -> Invoice {
  Invoice { id, org_id, amount }
}

fn item(name: &str, qty: i64) -> RepoItem {
  RepoItem {
    id: 0,
    name: name.into(),
    qty,
  }
}

// ---- statements per dialect (TestSqlClient) ----

fn connected() -> TestSqlClient {
  let test = TestSqlClient::new();
  assert!(matches!(test.connect().block_on(), Exit::Success(())));
  test
}

fn run_on<A: Send + 'static>(
  test: &TestSqlClient,
  eff: Effect<A, SqlError, SqlClientEnv>,
) -> Result<A, SqlError> {
  match eff
    .provide_all(hlist![SqlClientService::new(test.clone())])
    .block_on()
  {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn ledger_row(id: i64, title: &str, version: i64) -> SqlRow {
  SqlRow::from_pairs([
    ("id", SqlValue::I64(id)),
    ("title", title.into()),
    ("version", SqlValue::I64(version)),
  ])
}

#[test]
fn derive_maps_the_struct_to_its_table() {
  assert_eq!(
    (Ledger::NAME, Ledger::TABLE, Ledger::COLUMNS, Ledger::ID),
    (
      "Ledger",
      "repo_ledgers",
      &["id", "title", "version"][..],
      "id"
    )
  );
  assert_eq!(
    (Ledger::VERSION, Ledger::SOFT_DELETE, Ledger::TENANT),
    (Some("version"), None, None)
  );
  assert_eq!(Note::COLUMNS, ["id", "title_text", "archived"]);
  assert_eq!(Note::SOFT_DELETE, Some("deleted_at"));
  assert_eq!(Invoice::TENANT, Some("org_id"));
  assert_eq!(
    (RepoItem::TABLE, RepoItem::ID_GENERATED),
    ("repo_item", true)
  );
  assert_eq!(
    note(1, "a", true).values(),
    [
      ("id", SqlValue::I64(1)),
      ("title_text", "a".into()),
      ("archived", SqlValue::Bool(true)),
    ]
  );
  let mut l = Ledger {
    id: 1,
    title: "a".into(),
    version: 4,
  };
  l.bump_version();
  assert_eq!((l.id(), l.version), (1, 5));
  let mut i = item("x", 1);
  i.bump_version();
  assert_eq!(i, item("x", 1), "no version column: bump does nothing");
  assert!(RepoItem::scope().is_empty());
  assert!(!Note::scope().is_empty());
}

#[test]
fn postgres_statements_carry_every_filter() {
  let test = connected();
  let repo = Repository::<Note>::new(Dialect::Postgres);
  let live = r#""deleted_at" IS NULL AND "archived" = $1"#;
  test.script_query(
    format!(r#"SELECT COUNT(*) FROM "repo_notes" WHERE {live}"#),
    vec![SqlRow::from_pairs([("count", SqlValue::I64(2))])],
  );
  test.script_execute(
    r#"UPDATE "repo_notes" SET "title_text" = $1, "archived" = $2 WHERE "deleted_at" IS NULL AND "archived" = $3 AND "id" = $4"#,
    1,
  );
  assert_eq!(run_on(&test, repo.find(1)), Ok(None));
  assert_eq!(run_on(&test, repo.find_all()), Ok(vec![]));
  assert_eq!(
    run_on(&test, repo.find_by(Expr::col("title_text").eq("a"))),
    Ok(vec![])
  );
  assert_eq!(
    run_on(&test, repo.find_page(Condition::all(), Page::number(1, 10))),
    Ok(vec![])
  );
  assert_eq!(run_on(&test, repo.count()), Ok(2));
  assert_eq!(run_on(&test, repo.exists(1)), Ok(false));
  assert_eq!(
    run_on(&test, repo.update(note(1, "b", false))),
    Ok(note(1, "b", false))
  );
  assert_eq!(run_on(&test, repo.delete(1)), Ok(false));
  assert_eq!(run_on(&test, repo.hard_delete(1)), Ok(false));
  let queries: Vec<String> = test.recorded_queries().into_iter().map(|q| q.0).collect();
  assert_eq!(
    queries,
    [
      format!(
        r#"SELECT "id", "title_text", "archived" FROM "repo_notes" WHERE {live} AND "id" = $2 LIMIT $3"#
      ),
      format!(
        r#"SELECT "id", "title_text", "archived" FROM "repo_notes" WHERE {live} ORDER BY "id" ASC"#
      ),
      format!(
        r#"SELECT "id", "title_text", "archived" FROM "repo_notes" WHERE {live} AND "title_text" = $2 ORDER BY "id" ASC"#
      ),
      format!(
        r#"SELECT "id", "title_text", "archived" FROM "repo_notes" WHERE {live} ORDER BY "id" ASC LIMIT $2 OFFSET $3"#
      ),
      format!(r#"SELECT COUNT(*) FROM "repo_notes" WHERE {live}"#),
      format!(r#"SELECT "id" FROM "repo_notes" WHERE {live} AND "id" = $2 LIMIT $3"#),
    ]
  );
  let executes = test.recorded_executes();
  assert_eq!(
    executes[1].0,
    r#"UPDATE "repo_notes" SET "deleted_at" = $1 WHERE "deleted_at" IS NULL AND "archived" = $2 AND "id" = $3"#
  );
  assert!(matches!(executes[1].1[0], SqlValue::Timestamp(_)));
  assert_eq!(
    executes[2].0, r#"DELETE FROM "repo_notes" WHERE "archived" = $1 AND "id" = $2"#,
    "a hard delete reaches soft-deleted rows"
  );
  assert_eq!(
    test.recorded_queries()[3].1,
    [SqlValue::Bool(false), SqlValue::I64(10), SqlValue::I64(10)]
  );
}

#[test]
fn versioned_updates_check_and_bump_the_version() {
  let test = connected();
  let repo = Repository::<Ledger>::new(Dialect::Postgres);
  let sql = r#"UPDATE "repo_ledgers" SET "title" = $1, "version" = "version" + $2 WHERE "id" = $3 AND "version" = $4"#;
  test.script_execute(sql, 1);
  let saved = run_on(
    &test,
    repo.update(Ledger {
      id: 7,
      title: "b".into(),
      version: 2,
    }),
  );
  assert_eq!(saved.map(|l| l.version), Ok(3));
  assert_eq!(
    test.recorded_executes()[0].1,
    [
      SqlValue::Text("b".into()),
      SqlValue::I32(1),
      SqlValue::I64(7),
      SqlValue::I64(2)
    ]
  );

  // Zero rows and the row still there: someone else bumped the version.
  test.script_execute(sql, 0);
  test.script_query(
    r#"SELECT "id" FROM "repo_ledgers" WHERE "id" = $1 LIMIT $2"#,
    vec![SqlRow::from_pairs([("id", 7i64)])],
  );
  let stale = run_on(&test, repo.update(ledger(7, 2)));
  assert_eq!(
    stale,
    Err(SqlError::StaleVersion {
      entity: "Ledger".into(),
      id: "7".into()
    })
  );
  assert_eq!(
    stale.unwrap_err().to_string(),
    "sql stale version: Ledger 7 was changed by another writer"
  );
  // Zero rows and no row: not found.
  test.script_query(
    r#"SELECT "id" FROM "repo_ledgers" WHERE "id" = $1 LIMIT $2"#,
    vec![],
  );
  assert_eq!(
    run_on(&test, repo.update(ledger(7, 2))),
    Err(SqlError::RowNotFound)
  );
  // Without a version column zero rows is not found, with no second query.
  let items = Repository::<RepoItem>::new(Dialect::Postgres);
  assert_eq!(
    run_on(&test, items.update(item("x", 1))),
    Err(SqlError::RowNotFound)
  );
}

fn ledger(id: i64, version: i64) -> Ledger {
  Ledger {
    id,
    title: "t".into(),
    version,
  }
}

#[test]
fn soft_delete_also_bumps_the_version() {
  #[derive(Clone, Debug, PartialEq, FromRow, Entity)]
  #[entity(table = "repo_articles", soft_delete = "deleted_at")]
  struct Article {
    #[entity(id)]
    id: i64,
    #[entity(version)]
    version: i32,
  }
  let test = connected();
  let repo = Repository::<Article>::new(Dialect::Sqlite);
  assert_eq!(run_on(&test, repo.delete(1)), Ok(false));
  assert_eq!(
    test.recorded_executes()[0].0,
    r#"UPDATE "repo_articles" SET "deleted_at" = ?, "version" = "version" + ? WHERE "deleted_at" IS NULL AND "id" = ?"#
  );
}

#[test]
fn tenant_scope_fails_closed_and_is_written_on_insert() {
  let test = connected();
  let no_tenant = Repository::<Invoice>::new(Dialect::Sqlite);
  for err in [
    run_on(&test, no_tenant.find_all()).map(|_| ()),
    run_on(&test, no_tenant.insert(invoice(1, 1, 1))).map(|_| ()),
    run_on(&test, no_tenant.delete(1)).map(|_| ()),
  ] {
    assert!(
      matches!(err, Err(SqlError::Unsupported(ref m)) if m.contains("tenant column `org_id`")),
      "{err:?}"
    );
  }
  assert!(test.recorded_queries().is_empty() && test.recorded_executes().is_empty());

  let org = no_tenant.with_tenant(5i64);
  assert_eq!(org.tenant(), Some(&SqlValue::I64(5)));
  assert_eq!(org.dialect(), Dialect::Sqlite);
  let insert = r#"INSERT INTO "repo_invoices" ("id", "amount", "org_id") VALUES (?, ?, ?) RETURNING "id", "org_id", "amount""#;
  test.script_query(
    insert,
    vec![SqlRow::from_pairs([
      ("id", 1i64),
      ("org_id", 5),
      ("amount", 3),
    ])],
  );
  assert_eq!(
    run_on(&test, org.insert(invoice(1, 99, 3))),
    Ok(invoice(1, 5, 3))
  );
  assert_eq!(
    test.recorded_queries()[0].1,
    [SqlValue::I64(1), SqlValue::I64(3), SqlValue::I64(5)],
    "the repository's tenant replaces the field"
  );
  let _ = run_on(&test, org.update(invoice(1, 99, 4)));
  assert_eq!(
    test.recorded_executes()[0].0,
    r#"UPDATE "repo_invoices" SET "amount" = ? WHERE "org_id" = ? AND "id" = ?"#,
    "the tenant column is never updated"
  );

  // Unscoped: no filter, and the entity's own tenant value is written.
  let all = no_tenant.unscoped();
  let _ = run_on(&test, all.count());
  let _ = run_on(&test, all.insert(invoice(2, 8, 1)));
  let queries = test.recorded_queries();
  assert_eq!(queries[1].0, r#"SELECT COUNT(*) FROM "repo_invoices""#);
  assert_eq!(
    queries[2].0,
    r#"INSERT INTO "repo_invoices" ("id", "org_id", "amount") VALUES (?, ?, ?) RETURNING "id", "org_id", "amount""#
  );
  // An empty COUNT result (only a test double returns none) is not found.
  assert_eq!(run_on(&test, org.count()), Err(SqlError::RowNotFound));
  assert!(format!("{org:?}").contains("Invoice"));
}

#[test]
fn current_tenant_comes_from_the_environment() {
  let test = connected();
  let program =
    Repository::<Invoice>::for_current_tenant(Dialect::Postgres).flat_map(|repo| repo.find_all());
  let out = program
    .provide_all(hlist![
      SqlClientService::new(test.clone()),
      CurrentTenant::new(42i64)
    ])
    .block_on();
  assert!(
    matches!(out, Exit::Success(ref v) if v.is_empty()),
    "{out:?}"
  );
  assert_eq!(
    test.recorded_queries()[0],
    (
      r#"SELECT "id", "org_id", "amount" FROM "repo_invoices" WHERE "org_id" = $1 ORDER BY "id" ASC"#
        .to_owned(),
      vec![SqlValue::I64(42)]
    )
  );
}

#[test]
fn mysql_inserts_then_reads_back() {
  let test = connected();
  let repo = Repository::<Ledger>::new(Dialect::MySql);
  test.script_query(
    "SELECT `id`, `title`, `version` FROM `repo_ledgers` WHERE `id` = ?",
    vec![ledger_row(3, "t", 1)],
  );
  assert_eq!(run_on(&test, repo.insert(ledger(3, 1))), Ok(ledger(3, 1)));
  assert_eq!(
    test.recorded_executes()[0].0,
    "INSERT INTO `repo_ledgers` (`id`, `title`, `version`) VALUES (?, ?, ?)"
  );
  let generated = run_on(
    &test,
    Repository::<RepoItem>::new(Dialect::MySql).insert(item("x", 1)),
  );
  assert!(
    matches!(generated, Err(SqlError::Unsupported(ref m)) if m.contains("RETURNING")),
    "{generated:?}"
  );
  // RETURNING with no row back (only a test double does that) is not found.
  assert_eq!(
    run_on(
      &test,
      Repository::<Ledger>::new(Dialect::Sqlite).insert(ledger(3, 1))
    ),
    Err(SqlError::RowNotFound)
  );
}

#[test]
fn bound_repositories_run_on_any_executor() {
  let test = connected();
  let repo = Repository::<Ledger>::new(Dialect::Postgres);
  let find = r#"SELECT "id", "title", "version" FROM "repo_ledgers" WHERE "id" = $1 LIMIT $2"#;
  test.script_query(find, vec![ledger_row(1, "t", 1)]);
  let client: Arc<dyn SqlClient> = Arc::new(test.clone());
  let via_client = repo.on(Arc::clone(&client));
  assert!(format!("{via_client:?}").starts_with("BoundRepository"));
  assert_eq!(via_client.repository().dialect(), Dialect::Postgres);
  assert!(matches!(
    via_client.find(1).block_on(),
    Exit::Success(Some(ref l)) if *l == ledger(1, 1)
  ));
  let via_service = repo.on(SqlClientService(Arc::clone(&client)));
  assert!(matches!(
    via_service.exists(1).block_on(),
    Exit::Success(false)
  ));

  // In a transaction: committed statements are recorded, rolled-back ones are not.
  let tx_repo = repo.clone();
  let committed = with_transaction(Arc::clone(&client), move |tx: Arc<dyn SqlTransaction>| {
    let bound = tx_repo.on(SqlTransactionService(Arc::clone(&tx))).clone();
    bound
      .exists(1)
      .flat_map(move |_| tx_repo.on(tx).delete(1))
      .map_err(SqlError::from)
  });
  assert!(matches!(committed.block_on(), Exit::Success(false)));
  let failed = with_transaction(client, move |tx: Arc<dyn SqlTransaction>| {
    repo
      .on(tx)
      .hard_delete(2)
      .flat_map(|_| fail::<(), SqlError, _>(SqlError::RowNotFound))
  });
  assert!(matches!(failed.block_on(), Exit::Failure(_)));
  assert_eq!((test.commits(), test.rollbacks()), (1, 1));
  assert_eq!(test.recorded_executes().len(), 1);
  assert_eq!(Page::new(3, 6), Page::number(2, 3));
  assert_eq!(Page::number(u64::MAX, 2).offset, u64::MAX);
}

#[test]
fn custom_id_types_and_scope_errors_on_bound_repositories() {
  // Any `ToSqlValue + FromSqlValue + Clone + Debug` type is a key.
  #[derive(Debug, PartialEq, FromRow, Entity)]
  struct Big {
    #[entity(id)]
    id: Huge,
  }
  #[derive(Clone, Debug, PartialEq)]
  struct Huge;
  impl id_effect_sql::ToSqlValue for Huge {
    fn to_sql_value(&self) -> SqlValue {
      SqlValue::Null
    }
  }
  impl id_effect_sql::FromSqlValue for Huge {
    fn from_sql_value(_: &SqlValue) -> Result<Self, id_effect_sql::ValueError> {
      Ok(Huge)
    }
  }
  let test = connected();
  let repo = Repository::<Big>::new(Dialect::Postgres);
  assert_eq!(run_on(&test, repo.find(Huge)), Ok(None));
  let bound = repo.on(SqlClientService::new(test.clone()));
  assert!(matches!(bound.find(Huge).block_on(), Exit::Success(None)));
  let no_tenant = Repository::<Invoice>::new(Dialect::Postgres).on(SqlClientService::new(test));
  assert!(matches!(
    no_tenant.count().block_on(),
    Exit::Failure(Cause::Fail(SqlError::Unsupported(_)))
  ));
}

/// The exact `rustc -V` prefix the `tests/ui/entity/*.stderr` snapshots are blessed with: CI's
/// `moon-ci` pin, as in `tests/from_row.rs`. Other compilers skip the test (`UI_FORCE=1` runs it).
const BLESSED_RUSTC: &str = "rustc 1.99.0 ";

#[test]
// The expected rustc output for a non-integer version field depends on the toolchain only.
fn entity_derive_compile_errors() {
  let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
  let version = std::process::Command::new(rustc)
    .arg("-V")
    .output()
    .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    .unwrap_or_default();
  if !version.starts_with(BLESSED_RUSTC) && std::env::var_os("UI_FORCE").is_none() {
    eprintln!(
      "entity_derive_compile_errors: skipped on `{}`; snapshots are blessed on `{BLESSED_RUSTC}`",
      version.trim()
    );
    return;
  }
  let t = trybuild::TestCases::new();
  t.compile_fail("tests/ui/entity/*.rs");
}

// ---- the live suite ----

/// One database under test.
pub struct Db {
  client: Arc<dyn SqlClient>,
  dialect: Dialect,
}

impl Db {
  async fn run<A: Send + 'static>(
    &self,
    eff: Effect<A, SqlError, SqlClientEnv>,
  ) -> Result<A, SqlError> {
    let eff = eff.provide_all(hlist![SqlClientService(Arc::clone(&self.client))]);
    self.run_plain(eff).await
  }

  async fn run_plain<A: Send + 'static>(&self, eff: Effect<A, SqlError>) -> Result<A, SqlError> {
    match id_effect_tokio::TokioRuntime::current()
      .expect("tokio")
      .run(eff)
      .await
    {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  async fn ok<A: Send + 'static>(&self, eff: Effect<A, SqlError, SqlClientEnv>) -> A {
    self
      .run(eff)
      .await
      .unwrap_or_else(|e| panic!("statement failed: {e}"))
  }

  /// `DROP` then `CREATE` `table` with `columns` (PostgreSQL / SQLite column definitions).
  async fn create(&self, table: &str, pg: &str, sqlite: &str) {
    let columns = if self.dialect == Dialect::Postgres {
      pg
    } else {
      sqlite
    };
    for sql in [
      format!("DROP TABLE IF EXISTS {table}"),
      format!("CREATE TABLE {table} ({columns})"),
    ] {
      self
        .run_plain(self.client.execute(&sql, &[]))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
  }

  async fn drop_table(&self, table: &str) {
    let _ = self
      .run_plain(
        self
          .client
          .execute(&format!("DROP TABLE IF EXISTS {table}"), &[]),
      )
      .await;
  }
}

pub async fn stale_version_is_rejected(db: &Db) {
  let cols = "id BIGINT PRIMARY KEY, title TEXT NOT NULL, version BIGINT NOT NULL";
  db.create("repo_ledgers", cols, cols).await;
  let repo = Repository::<Ledger>::new(db.dialect);
  assert_eq!(db.ok(repo.insert(ledger(1, 1))).await, ledger(1, 1));

  let mine = db.ok(repo.find(1)).await.expect("row");
  let theirs = db.ok(repo.find(1)).await.expect("row");
  let saved = db
    .ok(repo.update(Ledger {
      title: "mine".into(),
      ..mine
    }))
    .await;
  assert_eq!(saved.version, 2);

  let lost = db
    .run(repo.update(Ledger {
      title: "theirs".into(),
      ..theirs
    }))
    .await;
  assert_eq!(
    lost,
    Err(SqlError::StaleVersion {
      entity: "Ledger".into(),
      id: "1".into()
    })
  );
  let stored = db.ok(repo.find(1)).await.expect("row");
  assert_eq!((stored.title.as_str(), stored.version), ("mine", 2));
  assert_eq!(
    db.run(repo.update(ledger(99, 1))).await,
    Err(SqlError::RowNotFound)
  );

  // The same check inside a transaction.
  let tx_repo = repo.clone();
  let in_tx = with_transaction(
    Arc::clone(&db.client),
    move |tx: Arc<dyn SqlTransaction>| {
      let on_tx = tx_repo.on(tx);
      on_tx.update(stored.clone()).flat_map(move |next| {
        on_tx
          .update(stored)
          .map(|_| unreachable!("the second writer must lose"))
          .catch(move |e| match e {
            SqlError::StaleVersion { .. } => id_effect::succeed(next),
            other => fail(other),
          })
      })
    },
  );
  assert_eq!(db.run_plain(in_tx).await.map(|l| l.version), Ok(3));
  db.drop_table("repo_ledgers").await;
}

pub async fn global_filter_hides_soft_deleted(db: &Db) {
  db.create(
    "repo_notes",
    "id BIGINT PRIMARY KEY, title_text TEXT NOT NULL, archived BOOLEAN NOT NULL, \
     deleted_at TIMESTAMPTZ",
    "id INTEGER PRIMARY KEY, title_text TEXT NOT NULL, archived BOOLEAN NOT NULL, \
     deleted_at TIMESTAMP",
  )
  .await;
  let repo = Repository::<Note>::new(db.dialect);
  for n in [note(1, "a", false), note(2, "b", false), note(3, "c", true)] {
    assert_eq!(db.ok(repo.unscoped().insert(n.clone())).await, n);
  }
  // The custom scope hides the archived note.
  assert_eq!(
    db.ok(repo.find_all()).await,
    [note(1, "a", false), note(2, "b", false)]
  );
  assert!(db.ok(repo.delete(1)).await);

  // Soft-deleted: hidden from every generated statement.
  assert_eq!(db.ok(repo.find(1)).await, None);
  assert!(!db.ok(repo.exists(1)).await);
  assert_eq!(db.ok(repo.count()).await, 1);
  assert_eq!(db.ok(repo.find_all()).await, [note(2, "b", false)]);
  assert_eq!(
    db.ok(repo.find_by(Expr::col("title_text").eq("a"))).await,
    []
  );
  assert_eq!(
    db.ok(repo.count_by(Expr::col("title_text").eq("a"))).await,
    0
  );
  assert_eq!(
    db.run(repo.update(note(1, "edited", false))).await,
    Err(SqlError::RowNotFound)
  );
  assert!(!db.ok(repo.delete(1)).await, "already deleted");

  // The escape hatches.
  assert_eq!(
    db.ok(repo.with_deleted().find(1)).await,
    Some(note(1, "a", false))
  );
  assert_eq!(db.ok(repo.with_deleted().count()).await, 2);
  assert_eq!(db.ok(repo.unscoped().count()).await, 3);
  let deleted_at = db
    .ok(id_effect_sql::query(
      "SELECT deleted_at FROM repo_notes WHERE id = 1",
      vec![],
    ))
    .await;
  assert!(
    !matches!(deleted_at[0].values()[0], SqlValue::Null),
    "soft delete stamps the row"
  );
  assert!(db.ok(repo.hard_delete(1)).await, "a hard delete purges it");
  assert_eq!(db.ok(repo.unscoped().count()).await, 2);
  db.drop_table("repo_notes").await;
}

pub async fn tenant_filter_scopes_every_query(db: &Db) {
  let cols = "id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, amount BIGINT NOT NULL";
  db.create("repo_invoices", cols, cols).await;
  let org1 = Repository::<Invoice>::new(db.dialect).with_tenant(1i64);
  let org2 = Repository::<Invoice>::new(db.dialect).with_tenant(2i64);
  // Inserts write the repository's tenant, whatever the field says.
  assert_eq!(
    db.ok(org1.insert(invoice(1, 99, 10))).await,
    invoice(1, 1, 10)
  );
  assert_eq!(
    db.ok(org2.insert(invoice(2, 1, 20))).await,
    invoice(2, 2, 20)
  );
  assert_eq!(
    db.ok(org2.insert(invoice(3, 2, 30))).await,
    invoice(3, 2, 30)
  );

  // Org 2 never sees, changes or deletes org 1's invoice.
  assert_eq!(db.ok(org2.find(1)).await, None);
  assert!(!db.ok(org2.exists(1)).await);
  assert_eq!(
    db.ok(org2.find_all()).await,
    [invoice(2, 2, 20), invoice(3, 2, 30)]
  );
  assert_eq!(db.ok(org2.count()).await, 2);
  assert_eq!(
    db.ok(org2.find_by(Expr::col("amount").gte(10))).await,
    [invoice(2, 2, 20), invoice(3, 2, 30)]
  );
  assert_eq!(
    db.ok(org2.find_page(Condition::all(), Page::new(1, 1)))
      .await,
    [invoice(3, 2, 30)]
  );
  assert_eq!(
    db.run(org2.update(invoice(1, 2, 0))).await,
    Err(SqlError::RowNotFound)
  );
  assert!(!db.ok(org2.delete(1)).await);
  assert!(!db.ok(org2.hard_delete(1)).await);
  assert_eq!(db.ok(org1.find(1)).await, Some(invoice(1, 1, 10)));

  // From the environment, as a request handler would.
  let from_env = Repository::<Invoice>::for_current_tenant(db.dialect)
    .flat_map(|repo| repo.count())
    .provide_all(hlist![
      SqlClientService(Arc::clone(&db.client)),
      CurrentTenant::new(1i64)
    ]);
  assert_eq!(db.run_plain(from_env).await, Ok(1));
  // No tenant fails closed; unscoped sees every tenant.
  assert!(matches!(
    db.run(Repository::<Invoice>::new(db.dialect).count()).await,
    Err(SqlError::Unsupported(_))
  ));
  assert_eq!(
    db.ok(Repository::<Invoice>::new(db.dialect).unscoped().count())
      .await,
    3
  );
  db.drop_table("repo_invoices").await;
}

pub async fn crud_paging_and_transactions(db: &Db) {
  db.create(
    "repo_item",
    "id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, qty BIGINT NOT NULL",
    "id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, qty BIGINT NOT NULL",
  )
  .await;
  let repo = Repository::<RepoItem>::new(db.dialect);
  let mut ids = Vec::new();
  for (n, name) in ["a", "b", "c", "d", "e"].into_iter().enumerate() {
    let stored = db.ok(repo.insert(item(name, n as i64))).await;
    assert_eq!((stored.name.as_str(), stored.qty), (name, n as i64));
    ids.push(stored.id);
  }
  assert!(
    ids.windows(2).all(|w| w[0] < w[1]),
    "generated ids: {ids:?}"
  );
  let page = db
    .ok(repo.find_page(Expr::col("qty").gte(0), Page::new(2, 2)))
    .await;
  assert_eq!(page.iter().map(|i| i.id).collect::<Vec<_>>(), ids[2..4]);
  let last = db
    .ok(repo.find_page(Condition::all(), Page::number(2, 2)))
    .await;
  assert_eq!(last.iter().map(|i| i.id).collect::<Vec<_>>(), ids[4..]);
  assert_eq!(db.ok(repo.count_by(Expr::col("qty").gt(2))).await, 2);

  let mut b = db.ok(repo.find(ids[1])).await.expect("b");
  b.qty = 40;
  assert_eq!(db.ok(repo.update(b.clone())).await, b);
  assert_eq!(db.ok(repo.find(ids[1])).await, Some(b));

  // A failed transaction rolls its insert back.
  let tx_repo = repo.clone();
  let failed = with_transaction(
    Arc::clone(&db.client),
    move |tx: Arc<dyn SqlTransaction>| {
      tx_repo
        .on(tx)
        .insert(item("rolled back", 0))
        .flat_map(|_| fail::<(), SqlError, _>(SqlError::RowNotFound))
    },
  );
  assert_eq!(db.run_plain(failed).await, Err(SqlError::RowNotFound));
  assert_eq!(db.ok(repo.count()).await, 5);

  assert!(db.ok(repo.delete(ids[0])).await);
  assert!(!db.ok(repo.exists(ids[0])).await);
  assert_eq!(db.ok(repo.count()).await, 4);
  db.drop_table("repo_item").await;
}

/// One `#[tokio::test]` per suite body for the backend `$make` builds (`async fn(&str) ->
/// Option<Db>`; `None` skips). Unused without a backend feature.
#[allow(unused_macros)]
macro_rules! repository_suite {
  ($make:path) => {
    #[tokio::test]
    async fn stale_version_is_rejected() {
      let Some(db) = $make("stale_version_is_rejected").await else {
        return;
      };
      crate::stale_version_is_rejected(&db).await;
    }

    #[tokio::test]
    async fn global_filter_hides_soft_deleted() {
      let Some(db) = $make("global_filter_hides_soft_deleted").await else {
        return;
      };
      crate::global_filter_hides_soft_deleted(&db).await;
    }

    #[tokio::test]
    async fn tenant_filter_scopes_every_query() {
      let Some(db) = $make("tenant_filter_scopes_every_query").await else {
        return;
      };
      crate::tenant_filter_scopes_every_query(&db).await;
    }

    #[tokio::test]
    async fn crud_paging_and_transactions() {
      let Some(db) = $make("crud_paging_and_transactions").await else {
        return;
      };
      crate::crud_paging_and_transactions(&db).await;
    }
  };
}

#[cfg(feature = "sqlite")]
mod sqlite {
  use std::sync::Arc;

  use id_effect_sql::Dialect;
  use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};

  use crate::Db;

  /// A fresh in-memory database.
  async fn memory(_test: &str) -> Option<Db> {
    let pool = sqlite_pool_from_config(SqlitePoolConfig::memory().with_max_size(4))
      .await
      .expect("sqlite pool");
    Some(Db {
      client: Arc::new(SqliteSqlClient::new(pool)),
      dialect: Dialect::Sqlite,
    })
  }

  repository_suite!(memory);
}

#[cfg(feature = "pg")]
mod pg {
  use std::sync::Arc;

  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{Dialect, SqlClient};

  use crate::Db;

  fn unavailable(test: &str, why: &str) -> Option<Db> {
    assert!(
      !std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1"),
      "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
    );
    eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
    None
  }

  /// A connected client at `DATABASE_URL`, or `None` (skip).
  async fn live_pg(test: &str) -> Option<Db> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
      return unavailable(test, "DATABASE_URL is not set");
    };
    let pool = match pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)).await {
      Ok(pool) => pool,
      Err(e) => return unavailable(test, &format!("cannot build pool: {}", e.0)),
    };
    let client = PgSqlClient::new(pool);
    let db = Db {
      client: Arc::new(client.clone()),
      dialect: Dialect::Postgres,
    };
    if let Err(e) = db.run_plain(client.connect()).await {
      return unavailable(test, &format!("cannot connect: {e:?}"));
    }
    Some(db)
  }

  repository_suite!(live_pg);
}
