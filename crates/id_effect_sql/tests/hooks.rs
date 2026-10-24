//! Entity lifecycle hooks (feature `repository`, data.orm #7, ADR 0015 D12): `EntityHooks` run
//! inside the write's transaction, in registration order; a failing hook rolls the write back; a
//! `before_*` hook rejects a write with the caller's typed error; with feature `outbox`, an
//! `after_update` hook stages an outbox event atomically with the update.
//!
//! The suite runs on SQLite in memory (feature `sqlite`) and on PostgreSQL (feature `pg`) in its
//! own container (`id_effect_test::Shared<Postgres>`, a random host port, a fresh database per
//! test, never the ambient `DATABASE_URL`). Without Docker the PostgreSQL tests print `SKIP` and
//! pass, unless `ID_EFFECT_REQUIRE_PG=1` or `ID_EFFECT_REQUIRE_DOCKER=1`.

#![cfg(feature = "repository")]
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use id_effect::{Cause, Effect, Exit, fail, hlist, succeed};
use id_effect_sql::repository::{EntityHooks, HookContext};
use id_effect_sql::{
  Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlClientService, SqlError,
  SqlTransaction, SqlValue, TestSqlClient, sql_client_env, with_transaction,
};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "hook_accounts", tenant = "org_id", soft_delete = "deleted_at")]
pub struct Account {
  #[entity(id)]
  pub id: i64,
  pub name: String,
  #[entity(version)]
  pub version: i64,
}

fn account(id: i64, name: &str) -> Account {
  Account {
    id,
    name: name.to_owned(),
    version: 0,
  }
}

/// The caller's own error type: hooks reject with it, SQL failures convert into it.
#[derive(Clone, Debug, PartialEq)]
pub enum AccountError {
  Sql(SqlError),
  Rejected(String),
}

impl From<SqlError> for AccountError {
  fn from(e: SqlError) -> Self {
    AccountError::Sql(e)
  }
}

/// What the hooks did, in order.
#[derive(Clone, Default)]
pub struct Trace(Arc<Mutex<Vec<String>>>);

impl Trace {
  fn push(&self, entry: impl Into<String>) {
    self.0.lock().expect("trace").push(entry.into());
  }

  fn take(&self) -> Vec<String> {
    std::mem::take(&mut *self.0.lock().expect("trace"))
  }
}

/// A statement on the write's transaction recording `entry` in `hook_log`.
fn log<T, E: From<SqlError> + Send + 'static>(ctx: &HookContext<T>, entry: &str) -> Effect<(), E>
where
  T: Entity,
{
  let sql = match ctx.dialect() {
    Dialect::Postgres => "INSERT INTO hook_log (entry) VALUES ($1)",
    _ => "INSERT INTO hook_log (entry) VALUES (?)",
  };
  ctx
    .transaction()
    .execute(sql, &[SqlValue::Text(entry.to_owned())])
    .map(|_| ())
    .map_err(E::from)
}

// ---- no database ----

#[test]
fn hooks_are_registered_per_repository() {
  let plain = Repository::<Account>::new(Dialect::Postgres).with_tenant(1i64);
  let empty = EntityHooks::<Account>::new();
  assert!(empty.is_empty());
  let hooks = EntityHooks::<Account>::new()
    .before_insert(|_, a| succeed(a))
    .after_insert(|_, _| succeed(()))
    .before_update(|_, a| succeed(a))
    .after_update(|_, _| succeed(()))
    .before_delete(|_, _| succeed(()))
    .after_delete(|_, _| succeed(()));
  assert!(!hooks.is_empty());
  let debug = format!("{:?}", hooks.clone());
  assert!(debug.contains("before_insert: 1"), "{debug}");
  let hooked = plain.with_hooks(hooks);
  assert_eq!(hooked.repository().tenant(), Some(&SqlValue::I64(1)));
  assert!(!hooked.hooks().is_empty());
  assert!(format!("{:?}", hooked.clone()).starts_with("HookedRepository"));
  // Another repository of the same entity carries other hooks (or none).
  let other = plain.with_hooks(EntityHooks::<Account>::new());
  assert!(other.hooks().is_empty());

  // The plain repository writes without hooks and without a transaction.
  let db = TestSqlClient::new();
  let _ = db.connect().block_on();
  db.script_execute(
    r#"UPDATE "hook_accounts" SET "name" = $1, "version" = "version" + $2 WHERE "org_id" = $3 AND "deleted_at" IS NULL AND "id" = $4 AND "version" = $5"#,
    1,
  );
  let updated = plain
    .update::<SqlClientEnv, _>(account(1, "a"))
    .provide_all(sql_client_env(db.clone()))
    .block_on();
  assert!(
    matches!(updated, Exit::Success(ref a) if a.version == 1),
    "{updated:?}"
  );
  assert_eq!((db.commits(), db.rollbacks()), (0, 0));

  // A hooked write opens a transaction, and a bound one runs in a savepoint of the caller's.
  let client: Arc<dyn SqlClient> = Arc::new(db.clone());
  let bound = hooked.clone();
  let program = with_transaction(client, move |tx| {
    let bound = bound.in_transaction(&tx);
    assert!(format!("{bound:?}").starts_with("BoundHookedRepository"));
    assert!(!bound.hooked().hooks().is_empty());
    bound.update(account(1, "b"))
  });
  assert!(matches!(program.block_on(), Exit::Success(_)));
  assert_eq!(db.recorded_executes().len(), 2);
  assert_eq!((db.commits(), db.rollbacks()), (1, 0));
}

#[test]
fn hook_context_is_bound_to_the_tenant() {
  let trace = Trace::default();
  let seen = trace.clone();
  let hooks = EntityHooks::<Account>::new().before_insert(move |ctx, a| {
    seen.push(format!(
      "{:?} {:?} {}",
      ctx.tenant(),
      ctx.dialect(),
      format!("{ctx:?}").starts_with("HookContext")
    ));
    assert_eq!(ctx.repository().repository().tenant(), ctx.tenant());
    succeed(a)
  });
  let db = TestSqlClient::new();
  let _ = db.connect().block_on();
  // The INSERT returns no row: the write fails after the hook ran, and is rolled back.
  let insert = Repository::<Account>::new(Dialect::Postgres)
    .with_tenant(7i64)
    .with_hooks(hooks)
    .insert::<SqlClientEnv, _>(account(1, "a"))
    .provide_all(sql_client_env(db.clone()))
    .block_on();
  assert!(
    matches!(insert, Exit::Failure(Cause::Fail(SqlError::RowNotFound))),
    "{insert:?}"
  );
  assert_eq!(trace.take(), ["Some(I64(7)) Postgres true"]);
  assert_eq!((db.commits(), db.rollbacks()), (0, 1));

  // A scope error (tenant-scoped entity, no tenant) fails the write inside the transaction.
  let no_tenant = Repository::<Account>::new(Dialect::Postgres)
    .with_hooks(EntityHooks::<Account, AccountError>::new())
    .delete::<SqlClientEnv, _>(1)
    .provide_all(hlist![SqlClientService::new(db.clone())])
    .block_on();
  assert!(
    matches!(
      no_tenant,
      Exit::Failure(Cause::Fail(AccountError::Sql(SqlError::Unsupported(_))))
    ),
    "{no_tenant:?}"
  );
}

#[cfg(feature = "outbox")]
#[test]
fn outbox_sinks_are_pluggable_and_none_stages_nothing() {
  use id_effect_sql::repository::outbox::{EntityOutbox, OutboxEvent};

  let staged = Trace::default();
  let seen = staged.clone();
  // Any closure over the write's transaction is a sink (e.g. id_effect_messaging's Outbox).
  let outbox = EntityOutbox::new(move |_tx: &Arc<dyn SqlTransaction>, e: OutboxEvent| {
    seen.push(e.topic().to_owned());
    succeed(())
  });
  let hooks = EntityHooks::<Account>::new().after_update(outbox.on_save(|a: &Account| {
    (a.name != "quiet").then(|| OutboxEvent::new("accounts.updated", Vec::new()))
  }));
  let repo = Repository::<Account>::new(Dialect::Postgres)
    .with_tenant(1i64)
    .with_hooks(hooks);
  let db = TestSqlClient::new();
  let _ = db.connect().block_on();
  db.script_execute(
    r#"UPDATE "hook_accounts" SET "name" = $1, "version" = "version" + $2 WHERE "org_id" = $3 AND "deleted_at" IS NULL AND "id" = $4 AND "version" = $5"#,
    1,
  );
  for name in ["loud", "quiet"] {
    let updated = repo
      .update::<SqlClientEnv, _>(account(1, name))
      .provide_all(sql_client_env(db.clone()))
      .block_on();
    assert!(matches!(updated, Exit::Success(_)), "{updated:?}");
  }
  assert_eq!(staged.take(), ["accounts.updated"]);
  assert_eq!((db.commits(), db.rollbacks()), (2, 0));
}

// ---- the live suite ----

/// One database under test.
pub struct Db {
  pub client: Arc<dyn SqlClient>,
  pub dialect: Dialect,
}

impl Db {
  pub async fn run_plain<A: Send + 'static, E: Send + 'static + std::fmt::Debug>(
    &self,
    eff: Effect<A, E>,
  ) -> Result<A, E> {
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

  pub async fn run<A: Send + 'static, E: Send + 'static + std::fmt::Debug>(
    &self,
    eff: Effect<A, E, SqlClientEnv>,
  ) -> Result<A, E> {
    let eff = eff.provide_all(hlist![SqlClientService(Arc::clone(&self.client))]);
    self.run_plain(eff).await
  }

  async fn exec(&self, sql: &str) {
    self
      .run_plain(SqlClient::execute(&*self.client, sql, &[]))
      .await
      .unwrap_or_else(|e| panic!("{sql}: {e}"));
  }

  async fn strings(&self, sql: &str) -> Vec<String> {
    let rows = self
      .run_plain(SqlClient::query(&*self.client, sql, &[]))
      .await
      .unwrap_or_else(|e| panic!("{sql}: {e}"));
    rows
      .iter()
      .map(|r| r.get::<String>(0usize).expect("text column"))
      .collect()
  }

  /// `id:name:version:deleted` of every account row, any tenant, deleted or not.
  pub async fn accounts(&self) -> Vec<String> {
    let sql = "SELECT CAST(id AS TEXT) || ':' || name || ':' || CAST(version AS TEXT) || ':' || \
               CASE WHEN deleted_at IS NULL THEN 'live' ELSE 'deleted' END FROM hook_accounts \
               ORDER BY id";
    self.strings(sql).await
  }

  /// The `hook_log` entries, in order.
  pub async fn log(&self) -> Vec<String> {
    self
      .strings("SELECT entry FROM hook_log ORDER BY seq")
      .await
  }

  /// `topic|key|payload` of every outbox row, in order.
  pub async fn outbox(&self) -> Vec<String> {
    let rows = self
      .run_plain(SqlClient::query(
        &*self.client,
        "SELECT topic, message_key, payload, message_id FROM id_effect_outbox ORDER BY id",
        &[],
      ))
      .await
      .expect("outbox");
    rows
      .iter()
      .map(|r| {
        let payload = r.get::<Vec<u8>>("payload").expect("payload");
        assert_eq!(r.get::<String>("message_id").expect("id").len(), 32);
        format!(
          "{}|{}|{}",
          r.get::<String>("topic").expect("topic"),
          r.get::<Option<String>>("message_key")
            .expect("key")
            .unwrap_or_default(),
          String::from_utf8(payload).expect("utf-8")
        )
      })
      .collect()
  }

  /// The tables: accounts (org 1's and one of org 2), the hook log and the outbox in
  /// id_effect_messaging's format.
  pub async fn seed(&self) {
    let pg = self.dialect == Dialect::Postgres;
    let (ts, serial, json, bytes) = if pg {
      (
        "TIMESTAMPTZ",
        "BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY",
        "JSONB",
        "BYTEA",
      )
    } else {
      (
        "TIMESTAMP",
        "INTEGER PRIMARY KEY AUTOINCREMENT",
        "TEXT",
        "BLOB",
      )
    };
    for table in ["hook_accounts", "hook_log", "id_effect_outbox"] {
      self.exec(&format!("DROP TABLE IF EXISTS {table}")).await;
    }
    self
      .exec(&format!(
        "CREATE TABLE hook_accounts (id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, \
         name TEXT NOT NULL, version BIGINT NOT NULL, deleted_at {ts})"
      ))
      .await;
    self
      .exec(&format!(
        "CREATE TABLE hook_log (seq {serial}, entry TEXT NOT NULL)"
      ))
      .await;
    self
      .exec(&format!(
        "CREATE TABLE id_effect_outbox (id {serial}, topic TEXT NOT NULL, \
         message_id TEXT NOT NULL, message_key TEXT, headers {json} NOT NULL, \
         payload {bytes} NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, \
         available_at {ts} NOT NULL, inserted_at {ts} NOT NULL, sent_at {ts}, dead_at {ts}, \
         locked_by TEXT, locked_until {ts}, last_error TEXT)"
      ))
      .await;
    self
      .exec("INSERT INTO hook_accounts (id, org_id, name, version) VALUES (99, 2, 'theirs', 0)")
      .await;
  }
}

fn accounts(db: &Db) -> Repository<Account> {
  Repository::<Account>::new(db.dialect).with_tenant(1i64)
}

pub async fn hooks_run_in_order_inside_transaction(db: &Db) {
  db.seed().await;
  let trace = Trace::default();
  let (t1, t2, t3, t4, t5, t6, t7) = (
    trace.clone(),
    trace.clone(),
    trace.clone(),
    trace.clone(),
    trace.clone(),
    trace.clone(),
    trace.clone(),
  );
  let hooks = EntityHooks::<Account>::new()
    .before_insert(move |_, mut a| {
      t1.push("before_insert#1");
      a.name = a.name.trim().to_owned();
      succeed(a)
    })
    .before_insert(move |_, a| {
      t2.push(format!("before_insert#2 {}", a.name));
      succeed(a)
    })
    .after_insert(move |ctx, a| {
      let trace = t3.clone();
      let id = a.id;
      // The row is visible in the write's transaction; another tenant's row is not.
      let mine = ctx.repository().find(id);
      let theirs = ctx.repository().find(99);
      let logged = log(ctx, "after_insert");
      mine.zip(theirs).flat_map(move |(mine, theirs)| {
        trace.push(format!(
          "after_insert sees_row={} sees_other_tenant={}",
          mine.is_some(),
          theirs.is_some()
        ));
        logged
      })
    })
    .before_update(move |_, a| {
      t4.push(format!("before_update v{}", a.version));
      succeed(a)
    })
    .after_update(move |ctx, a| {
      t5.push(format!("after_update v{}", a.version));
      log(ctx, "after_update")
    })
    .before_delete(move |ctx, id| {
      t6.push(format!("before_delete {id}"));
      log(ctx, "before_delete")
    })
    .after_delete(move |ctx, id| {
      t7.push(format!("after_delete {id}"));
      log(ctx, "after_delete")
    });
  let repo = accounts(db).with_hooks(hooks);

  let stored = db.run(repo.insert(account(1, "  ada "))).await.unwrap();
  assert_eq!(stored, account(1, "ada"));
  let updated = db
    .run(repo.update(account(1, "ada lovelace")))
    .await
    .unwrap();
  assert_eq!(updated.version, 1);
  assert!(db.run(repo.delete(1)).await.unwrap());
  // No row deleted: the after_delete hooks do not run.
  assert!(!db.run(repo.hard_delete(42)).await.unwrap());
  assert_eq!(
    trace.take(),
    [
      "before_insert#1",
      "before_insert#2 ada",
      "after_insert sees_row=true sees_other_tenant=false",
      "before_update v0",
      "after_update v1",
      "before_delete 1",
      "after_delete 1",
      "before_delete 42",
    ]
  );
  assert_eq!(
    db.log().await,
    [
      "after_insert",
      "after_update",
      "before_delete",
      "after_delete",
      "before_delete"
    ]
  );
  assert_eq!(
    db.accounts().await,
    ["1:ada lovelace:2:deleted", "99:theirs:0:live"]
  );

  // Inside the caller's transaction: the same hooks, in a savepoint; the caller commits.
  let bound_repo = repo.clone();
  let program = with_transaction(
    Arc::clone(&db.client),
    move |tx: Arc<dyn SqlTransaction>| {
      let hooked = bound_repo.in_transaction(&tx);
      let (soft, hard) = (hooked.clone(), hooked.clone());
      hooked
        .insert(account(2, " bob"))
        .flat_map(move |bob| soft.delete(bob.id))
        .flat_map(move |deleted| hard.hard_delete(2).map(move |gone| deleted && gone))
    },
  );
  assert!(db.run_plain(program).await.unwrap());
  assert_eq!(
    trace.take(),
    [
      "before_insert#1",
      "before_insert#2 bob",
      "after_insert sees_row=true sees_other_tenant=false",
      "before_delete 2",
      "after_delete 2",
      "before_delete 2",
      "after_delete 2",
    ]
  );
  assert_eq!(
    db.accounts().await,
    ["1:ada lovelace:2:deleted", "99:theirs:0:live"]
  );
}

pub async fn hook_failure_rolls_back_write(db: &Db) {
  db.seed().await;
  let fail_after = |event: &'static str| {
    move |ctx: &HookContext<Account>| {
      log::<Account, SqlError>(ctx, event)
        .flat_map(move |()| fail(SqlError::Unsupported(format!("{event} hook failed"))))
    }
  };
  let (on_insert, on_update, on_delete) = (
    fail_after("after_insert"),
    fail_after("after_update"),
    fail_after("after_delete"),
  );
  let failing = accounts(db).with_hooks(
    EntityHooks::<Account>::new()
      .after_insert(move |ctx, _| on_insert(ctx))
      .after_update(move |ctx, _| on_update(ctx))
      .after_delete(move |ctx, _| on_delete(ctx)),
  );
  let plain = accounts(db);

  let err = db.run(failing.insert(account(1, "ada"))).await.unwrap_err();
  assert_eq!(
    err,
    SqlError::Unsupported("after_insert hook failed".into())
  );
  assert_eq!(
    db.accounts().await,
    ["99:theirs:0:live"],
    "insert rolled back"
  );

  db.run(plain.insert(account(1, "ada"))).await.unwrap();
  let err = db.run(failing.update(account(1, "eve"))).await.unwrap_err();
  assert_eq!(
    err,
    SqlError::Unsupported("after_update hook failed".into())
  );
  let err = db.run(failing.delete(1)).await.unwrap_err();
  assert_eq!(
    err,
    SqlError::Unsupported("after_delete hook failed".into())
  );
  let err = db.run(failing.hard_delete(1)).await.unwrap_err();
  assert_eq!(
    err,
    SqlError::Unsupported("after_delete hook failed".into())
  );
  assert_eq!(
    db.accounts().await,
    ["1:ada:0:live", "99:theirs:0:live"],
    "update and deletes rolled back"
  );
  assert!(
    db.log().await.is_empty(),
    "the hooks' own statements rolled back"
  );

  // In the caller's transaction: the failing write is undone (savepoint) even though the caller
  // catches the error and commits the rest of its work.
  let (hooked, other) = (failing.clone(), plain.clone());
  let program = with_transaction(
    Arc::clone(&db.client),
    move |tx: Arc<dyn SqlTransaction>| {
      let mine = other.on(Arc::clone(&tx));
      let attempt = hooked
        .in_transaction(&tx)
        .insert(account(2, "bob"))
        .map(|_| "inserted")
        .catch(|_| succeed("caught"));
      attempt.flat_map(move |outcome| mine.insert(account(3, "cy")).map(move |_| outcome))
    },
  );
  assert_eq!(db.run_plain(program).await.unwrap(), "caught");
  assert_eq!(
    db.accounts().await,
    ["1:ada:0:live", "3:cy:0:live", "99:theirs:0:live"]
  );
  assert!(db.log().await.is_empty());
}

pub async fn before_hook_can_reject_with_typed_error(db: &Db) {
  db.seed().await;
  let hooks = EntityHooks::<Account, AccountError>::new()
    .before_insert(|_, a| {
      if a.name.is_empty() {
        fail(AccountError::Rejected("name is empty".into()))
      } else {
        succeed(a)
      }
    })
    .before_update(|ctx, a| {
      // A hook may read through the bound repository before deciding.
      let name = a.name.clone();
      ctx
        .repository()
        .exists(a.id)
        .map_err(AccountError::from)
        .flat_map(move |exists| {
          if name == "root" {
            fail(AccountError::Rejected(format!(
              "reserved name (row exists: {exists})"
            )))
          } else {
            succeed(a)
          }
        })
    })
    .before_delete(|_, id| {
      if *id == 1 {
        fail(AccountError::Rejected("account 1 is protected".into()))
      } else {
        succeed(())
      }
    });
  let repo = accounts(db).with_hooks(hooks);

  let err = db.run(repo.insert(account(1, ""))).await.unwrap_err();
  assert_eq!(err, AccountError::Rejected("name is empty".into()));
  assert_eq!(db.accounts().await, ["99:theirs:0:live"]);

  db.run(repo.insert(account(1, "ada"))).await.unwrap();
  let err = db.run(repo.update(account(1, "root"))).await.unwrap_err();
  assert_eq!(
    err,
    AccountError::Rejected("reserved name (row exists: true)".into())
  );
  let err = db.run(repo.delete(1)).await.unwrap_err();
  assert_eq!(err, AccountError::Rejected("account 1 is protected".into()));
  // SQL failures arrive as the same error type.
  let err = db.run(repo.update(account(7, "nobody"))).await.unwrap_err();
  assert_eq!(err, AccountError::Sql(SqlError::RowNotFound));
  // Another tenant's row is out of reach of the hooked repository too.
  let err = db
    .run(repo.update(account(99, "mine now")))
    .await
    .unwrap_err();
  assert_eq!(err, AccountError::Sql(SqlError::RowNotFound));
  assert_eq!(db.accounts().await, ["1:ada:0:live", "99:theirs:0:live"]);
}

#[cfg(feature = "outbox")]
pub async fn after_update_writes_outbox_event_atomically(db: &Db) {
  use id_effect_sql::repository::outbox::{EntityOutbox, OutboxEvent, OutboxTable};

  db.seed().await;
  let outbox = EntityOutbox::new(OutboxTable::new(db.dialect));
  let event = |a: &Account| {
    Some(
      OutboxEvent::json(
        "accounts.updated",
        &serde_json::json!({ "id": a.id, "name": a.name, "version": a.version }),
      )
      .with_key(a.id.to_string()),
    )
  };
  let repo = accounts(db).with_hooks(
    EntityHooks::<Account>::new()
      .after_update(outbox.on_save(event))
      .after_delete(outbox.on_delete(|id: &i64| {
        (*id != 3).then(|| OutboxEvent::new("accounts.deleted", id.to_string()))
      })),
  );
  db.run(accounts(db).insert(account(1, "ada")))
    .await
    .unwrap();
  db.run(repo.update(account(1, "ada lovelace")))
    .await
    .unwrap();
  assert_eq!(
    db.outbox().await,
    [r#"accounts.updated|1|{"id":1,"name":"ada lovelace","version":1}"#]
  );

  // A later hook fails: the update and its event roll back together.
  let failing = accounts(db).with_hooks(
    EntityHooks::<Account>::new()
      .after_update(outbox.on_save(event))
      .after_update(|_, _| fail(SqlError::Unsupported("downstream failed".into()))),
  );
  let err = db
    .run(
      failing
        .update(Account {
          version: 1,
          ..account(1, "eve")
        })
        .map(|_| ()),
    )
    .await
    .unwrap_err();
  assert_eq!(err, SqlError::Unsupported("downstream failed".into()));
  // Staging fails (a topic the relay could not publish): the update rolls back.
  let bad_topic = accounts(db).with_hooks(
    EntityHooks::<Account>::new()
      .after_update(outbox.on_save(|_: &Account| Some(OutboxEvent::new("not a topic", "x")))),
  );
  let err = db
    .run(
      bad_topic
        .update(Account {
          version: 1,
          ..account(1, "eve")
        })
        .map(|_| ()),
    )
    .await
    .unwrap_err();
  assert!(
    matches!(&err, SqlError::Unsupported(m) if m.starts_with("invalid outbox topic")),
    "{err:?}"
  );
  assert_eq!(
    db.accounts().await,
    ["1:ada lovelace:1:live", "99:theirs:0:live"]
  );
  assert_eq!(db.outbox().await.len(), 1, "no event without its write");

  // Deletes stage through on_delete; `None` stages nothing.
  db.run(accounts(db).insert(account(3, "cy"))).await.unwrap();
  assert!(db.run(repo.delete(1)).await.unwrap());
  assert!(db.run(repo.hard_delete(3)).await.unwrap());
  assert_eq!(db.outbox().await[1..], ["accounts.deleted||1".to_owned()]);
}

/// One `#[tokio::test]` per suite body for the backend `$make` builds (`async fn(&str) ->
/// Option<Db>`; `None` skips).
macro_rules! hooks_suite {
  ($make:path) => {
    hooks_suite!(@one $make, hooks_run_in_order_inside_transaction);
    hooks_suite!(@one $make, hook_failure_rolls_back_write);
    hooks_suite!(@one $make, before_hook_can_reject_with_typed_error);
    #[cfg(feature = "outbox")]
    hooks_suite!(@one $make, after_update_writes_outbox_event_atomically);
  };
  (@one $make:path, $name:ident) => {
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn $name() {
      let Some(db) = $make(stringify!($name)).await else {
        return;
      };
      super::$name(&db).await;
    }
  };
}

#[cfg(feature = "sqlite")]
mod sqlite {
  use std::sync::Arc;

  use id_effect_sql::Dialect;
  use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};

  use super::Db;

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

  hooks_suite!(memory);
}

#[cfg(feature = "pg")]
mod pg {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  use id_effect::{Cause, Exit};
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{Dialect, SqlClient};
  use id_effect_test::{ContainerError, Postgres, Shared, docker_available};

  use super::Db;

  static PG: Shared<Postgres> = Shared::new(Postgres::new);

  fn required() -> bool {
    ["ID_EFFECT_REQUIRE_PG", "ID_EFFECT_REQUIRE_DOCKER"]
      .iter()
      .any(|var| std::env::var(var).is_ok_and(|v| v == "1"))
  }

  /// A connected client on a fresh database of the shared container, or `None` (skip).
  async fn live_pg(test: &str) -> Option<Db> {
    if let Err(e) = docker_available() {
      assert!(
        !required(),
        "{test}: ID_EFFECT_REQUIRE_PG=1 but PostgreSQL cannot start: {e}"
      );
      eprintln!("SKIP {test}: {e} (ID_EFFECT_REQUIRE_PG=1 to require it)");
      return None;
    }
    let runtime = id_effect_tokio::TokioRuntime::current().expect("tokio");
    let pg = match runtime.run(PG.get::<ContainerError, _>()).await {
      Exit::Success(pg) => pg,
      Exit::Failure(Cause::Fail(e)) => panic!("{test}: PostgreSQL container: {e}"),
      Exit::Failure(c) => panic!("{test}: PostgreSQL container: {c:?}"),
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let database = format!(
      "hooks_{}_{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let admin = connect(&pg.url()).await;
    let created = runtime
      .run(admin.execute(&format!("CREATE DATABASE {database}"), &[]))
      .await;
    assert!(
      matches!(created, Exit::Success(_)),
      "{test}: CREATE DATABASE: {created:?}"
    );
    let _ = runtime.run(admin.close()).await;
    Some(Db {
      client: Arc::new(connect(&pg.url_for(&database)).await),
      dialect: Dialect::Postgres,
    })
  }

  async fn connect(url: &str) -> PgSqlClient {
    let config = PgPoolConfig::from_url(url.to_owned()).with_max_size(2);
    let pool = pg_pool_from_config(config)
      .await
      .unwrap_or_else(|e| panic!("pool for {url}: {}", e.0));
    let client = PgSqlClient::new(pool);
    let connected = id_effect_tokio::TokioRuntime::current()
      .expect("tokio")
      .run(client.connect())
      .await;
    assert!(matches!(connected, Exit::Success(())), "{connected:?}");
    client
  }

  hooks_suite!(live_pg);
}
