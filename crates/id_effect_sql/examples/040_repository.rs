//! Entities and repositories end to end: `#[derive(Entity)]`, `Repository<T>` CRUD and paging,
//! optimistic locking, soft delete plus a custom scope, a repository bound to a transaction, and a
//! report written as a sea-query statement next to it.
//!
//! It runs on a private in-memory SQLite database (feature `sqlite`), so no server is needed; the
//! same code runs on PostgreSQL with `Dialect::Postgres` and a `PgSqlClient`.
//!
//! Run: `cargo run -p id_effect_sql --example 040_repository --features repository,sqlite`

use std::process::ExitCode;
use std::sync::Arc;

use id_effect::{Cause, Effect, Exit, fail, hlist};
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait, Func, Order, Query};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{
  Dialect, Entity, FromRow, Page, Repository, SqlClient, SqlClientEnv, SqlClientService, SqlError,
  SqlTransaction, fetch_all_as, with_transaction,
};
use id_effect_tokio::TokioRuntime;

/// The custom scope of [`Account`]: archived accounts are hidden like deleted ones.
fn not_archived() -> Expr {
  Expr::col("archived").eq(false)
}

/// A table row: the columns are the `FromRow` columns, the database assigns the id, `version` is
/// checked and bumped by every update, and `deleted_at` (not a field) marks deleted rows.
#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "accounts", soft_delete = "deleted_at", scope = not_archived)]
struct Account {
  #[entity(id, generated)]
  id: i64,
  owner: String,
  balance: i64,
  archived: bool,
  #[entity(version)]
  version: i64,
}

fn account(owner: &str, balance: i64) -> Account {
  Account {
    id: 0, // assigned by the database
    owner: owner.into(),
    balance,
    archived: false,
    version: 1,
  }
}

const SCHEMA: &str = "CREATE TABLE accounts (
  id INTEGER PRIMARY KEY,
  owner TEXT NOT NULL,
  balance BIGINT NOT NULL,
  archived BOOLEAN NOT NULL,
  version BIGINT NOT NULL,
  deleted_at TIMESTAMP
)";

/// A row of the balance report (a statement of our own, not an entity).
#[derive(Debug, PartialEq, FromRow)]
struct OwnerTotal {
  owner: String,
  total: i64,
}

/// A transfer's failure: the database's, or the business rule's.
#[derive(Debug)]
enum TransferError {
  #[allow(dead_code)] // read through `Debug`
  Sql(SqlError),
  InsufficientFunds {
    balance: i64,
    amount: i64,
  },
}

impl From<SqlError> for TransferError {
  fn from(e: SqlError) -> Self {
    TransferError::Sql(e)
  }
}

/// The in-memory database (it lives as long as the pool).
struct Db {
  client: Arc<dyn SqlClient>,
}

impl Db {
  async fn open() -> Result<Self, SqlError> {
    let pool = sqlite_pool_from_config(SqlitePoolConfig::memory())
      .await
      .map_err(|e| e.0)?;
    let client: Arc<dyn SqlClient> = Arc::new(SqliteSqlClient::new(pool));
    let db = Self { client };
    db.plain(db.client.execute(SCHEMA, &[])).await?;
    Ok(db)
  }

  /// Runs `eff` with the client as its environment.
  async fn run<A, E>(&self, eff: Effect<A, E, SqlClientEnv>) -> Result<A, E>
  where
    A: Send + 'static,
    E: std::fmt::Debug + Send + 'static,
  {
    let client = SqlClientService(Arc::clone(&self.client));
    self.plain(eff.provide_all(hlist![client])).await
  }

  async fn plain<A, E>(&self, eff: Effect<A, E>) -> Result<A, E>
  where
    A: Send + 'static,
    E: std::fmt::Debug + Send + 'static,
  {
    match TokioRuntime::current().expect("tokio").run(eff).await {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(cause) => panic!("not a typed failure: {cause:?}"),
    }
  }
}

/// Moves `amount` from `from` to `to` in one transaction: both updates commit, or neither does.
fn transfer(
  db: &Db,
  repo: &Repository<Account>,
  from: i64,
  to: i64,
  amount: i64,
) -> Effect<(Account, Account), TransferError> {
  let repo = repo.clone();
  with_transaction(
    Arc::clone(&db.client),
    move |tx: Arc<dyn SqlTransaction>| {
      let accounts = repo.on(tx);
      let both = accounts.find(from).flat_map({
        let accounts = accounts.clone();
        move |a| accounts.find(to).map(move |b| (a, b))
      });
      both.map_err(TransferError::from).flat_map(move |found| {
        let (Some(mut a), Some(mut b)) = found else {
          return fail(TransferError::Sql(SqlError::RowNotFound));
        };
        if a.balance < amount {
          let balance = a.balance;
          return fail(TransferError::InsufficientFunds { balance, amount });
        }
        a.balance -= amount;
        b.balance += amount;
        let accounts2 = accounts.clone();
        accounts
          .update(a)
          .flat_map(move |a| accounts2.update(b).map(move |b| (a, b)))
          .map_err(TransferError::from)
      })
    },
  )
}

async fn steps(db: &Db) -> Result<(), SqlError> {
  let repo = Repository::<Account>::new(Dialect::Sqlite);

  // Insert: the generated id and the stored row come back.
  let mut ids = Vec::new();
  for (owner, balance) in [("ada", 100), ("ada", 40), ("bob", 70), ("cy", 10)] {
    let stored = db.run(repo.insert(account(owner, balance))).await?;
    println!("insert: {stored:?}");
    ids.push(stored.id);
  }
  let (ada1, ada2, bob, cy) = (ids[0], ids[1], ids[2], ids[3]);

  // Find by id, by condition, a page, a count.
  let found = db.run(repo.find(bob)).await?.expect("bob's account");
  assert_eq!((found.owner.as_str(), found.balance), ("bob", 70));
  let rich = db.run(repo.find_by(Expr::col("balance").gte(50))).await?;
  assert_eq!(rich.iter().map(|a| a.id).collect::<Vec<_>>(), [ada1, bob]);
  let second_page = db
    .run(repo.find_page(Expr::col("owner").ne("nobody"), Page::number(1, 2)))
    .await?;
  assert_eq!(
    second_page.iter().map(|a| a.id).collect::<Vec<_>>(),
    [bob, cy]
  );
  assert_eq!(db.run(repo.count()).await?, 4);
  println!(
    "find_by balance >= 50: {} rows; page 1 of size 2: {} rows",
    rich.len(),
    second_page.len()
  );

  // Optimistic locking: two writers read version 1; the second update loses.
  let mine = db.run(repo.find(cy)).await?.expect("row");
  let theirs = mine.clone();
  let saved = db
    .run(repo.update(Account {
      balance: 15,
      ..mine
    }))
    .await?;
  assert_eq!(saved.version, 2);
  let lost = db
    .run(repo.update(Account {
      balance: 999,
      ..theirs
    }))
    .await;
  match lost {
    Err(SqlError::StaleVersion { entity, id }) => {
      println!("update: stale version for {entity} {id}; reload and retry");
    }
    other => panic!("expected a stale version, got {other:?}"),
  }
  let fresh = db.run(repo.find(cy)).await?.expect("row");
  assert_eq!((fresh.balance, fresh.version), (15, 2));

  // A transfer is one transaction over the same repository.
  match db.plain(transfer(db, &repo, ada1, bob, 30)).await {
    Ok((a, b)) => println!("transfer: ada {} -> bob {}", a.balance, b.balance),
    Err(e) => panic!("transfer failed: {e:?}"),
  }
  match db.plain(transfer(db, &repo, cy, bob, 500)).await {
    Err(TransferError::InsufficientFunds { balance, amount }) => {
      println!("transfer: refused ({amount} > {balance}), nothing written");
    }
    other => panic!("expected insufficient funds, got {other:?}"),
  }
  let bob_now = db.run(repo.find(bob)).await?.expect("row");
  assert_eq!((bob_now.balance, bob_now.version), (100, 2));

  // Soft delete and the custom scope: hidden from every statement the repository builds.
  assert!(db.run(repo.delete(ada2)).await?);
  let archived = db.run(repo.find(ada1)).await?.expect("row");
  db.run(repo.update(Account {
    archived: true,
    ..archived
  }))
  .await?;
  assert_eq!(db.run(repo.find(ada2)).await?, None);
  assert_eq!(
    db.run(repo.find(ada1)).await?,
    None,
    "archived: out of scope"
  );
  assert_eq!(db.run(repo.count()).await?, 2);
  assert_eq!(
    db.run(repo.with_deleted().count()).await?,
    3,
    "deleted, not archived"
  );
  assert_eq!(db.run(repo.unscoped().count()).await?, 4);
  println!("soft delete: 2 visible, 3 with deleted, 4 unscoped");

  // A statement of your own: the repository's filters are not added to it.
  let report = Query::select()
    .column("owner")
    .expr_as(Func::sum(Expr::col("balance")), "total")
    .from("accounts")
    .and_where(Expr::col("deleted_at").is_null())
    .group_by_col("owner")
    .order_by("owner", Order::Asc)
    .to_owned();
  let totals = db
    .run(fetch_all_as::<OwnerTotal, _, _>(Dialect::Sqlite, &report))
    .await?;
  assert_eq!(
    totals,
    [
      OwnerTotal {
        owner: "ada".into(),
        total: 70
      },
      OwnerTotal {
        owner: "bob".into(),
        total: 100
      },
      OwnerTotal {
        owner: "cy".into(),
        total: 15
      },
    ]
  );
  println!("report: {totals:?}");

  // `hard_delete` removes the row for good.
  assert!(db.run(repo.hard_delete(ada2)).await?);
  assert_eq!(db.run(repo.unscoped().count()).await?, 3);
  Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
  let db = match Db::open().await {
    Ok(db) => db,
    Err(e) => {
      eprintln!("040_repository: cannot open SQLite: {e}");
      return ExitCode::from(2);
    }
  };
  match steps(&db).await {
    Ok(()) => {
      println!(
        "040_repository ok (SQLite in memory: CRUD, paging, versions, transaction, filters)"
      );
      ExitCode::SUCCESS
    }
    Err(e) => {
      eprintln!("040_repository failed: {e}");
      ExitCode::FAILURE
    }
  }
}
