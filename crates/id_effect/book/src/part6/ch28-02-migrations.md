# Migrations

A schema changes with the code that uses it. `id_effect_sql::migrate` (feature `migrate`) keeps those changes as **versioned SQL scripts**: each is applied once, in order, and recorded in a tracking table, so every database (laptop, CI, staging, production) can say which scripts it has and whether they are the ones in the repository. A script with a `.down.sql` partner can be **reverted**. The same engine runs as a startup layer of your service, from the `id-effect migrate` command, and as plain `Effect`s.

```toml
id_effect_sql = { version = "0.13", features = ["pg", "migrate"] }
```

The engine speaks PostgreSQL (advisory lock, `BYTEA`, `TIMESTAMPTZ`, `$n` placeholders). It runs on any `SqlClient`, so the in-memory `TestSqlClient` exercises it without a database.

## Files and naming

Migrations live in a directory, `migrations/` by default, one file per version. The names are sqlx-cli's, so a directory works with both tools:

| File | Meaning |
|---|---|
| `0001_init.sql` | version 1, description `init`; cannot be reverted |
| `0002_add_tags.up.sql` + `0002_add_tags.down.sql` | version 2, reversible: the down script undoes the up script |
| `20261016120000_add_users.up.sql` | a timestamp version (`YYYYMMDDHHMMSS`, UTC) |

The version is the integer before the first `_`; the description is the rest, with `_` read as a space. Versions are applied in ascending numeric order, and files that do not look like migrations (a `README.md`) are ignored. Sequential versions (`0001`, `0002`) read well in a small team; timestamps avoid two branches picking the same number. The `service` template of `id-effect new` starts with a sequential `migrations/0001_init.sql`.

`Migrator::from_dir("migrations")` reads a directory. `Migrator::from_files` takes `(file name, script)` pairs named the same way, which is how the examples on this page avoid a directory:

```rust
use id_effect_sql::migrate::{MigrateError, Migrator};

let migrator = Migrator::from_files([
    ("0001_init.sql", "CREATE TABLE todos (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL)"),
    ("0002_add_tags.up.sql", "ALTER TABLE todos ADD COLUMN tags TEXT NOT NULL DEFAULT ''"),
    ("0002_add_tags.down.sql", "ALTER TABLE todos DROP COLUMN tags"),
    ("README.md", "not a migration: ignored"),
])?;

let m = migrator.migrations(); // ascending by version
assert_eq!(m.len(), 2);
assert_eq!((m[0].version(), m[0].description(), m[0].is_reversible()), (1, "init", false));
assert_eq!(m[1].file(), "0002_add_tags.up.sql");
assert_eq!(m[1].down_file(), Some("0002_add_tags.down.sql"));
assert_eq!(m[1].checksum().len(), 48); // SHA-384 of the up script, as sqlx-cli computes it

// A broken source is a `MigrateError::Source` before anything touches a database.
let twice = Migrator::from_files([("1_a.sql", "SELECT 1"), ("01_b.sql", "SELECT 2")]);
assert!(matches!(twice, Err(MigrateError::Source(ref why)) if why.contains("same version 1")));
let orphan = Migrator::from_files([("3_c.down.sql", "DROP TABLE c")]);
assert!(matches!(orphan, Err(MigrateError::Source(ref why)) if why.contains("has no up migration")));
# Ok::<(), MigrateError>(())
```

The builders `with_table("app.schema_migrations")` (default `_id_effect_migrations`), `with_lock_key(key)` and `with_lock_timeout(duration)` (default 60 s) change where and how the migrations are tracked; every migrator of one database must agree on them.

## Writing a migration: `id-effect migrate new`

```sh
id-effect migrate new add_tags --reversible   # migrations/0002_add_tags.up.sql + .down.sql
id-effect migrate new add_users               # migrations/0003_add_users.sql
id-effect migrate new audit_log --timestamp   # migrations/20261016120000_audit_log.sql
```

`new` numbers after what is in `--dir` (default `migrations`): sequential versions continue at their width, and an empty directory or one of timestamps gets the current UTC time, like sqlx-cli. `--timestamp` / `--sequential` force a scheme and `-r` / `--reversible` writes the up/down pair. Then write the SQL. A script may hold several statements: the engine splits it, respecting strings, quoted identifiers, `$$`-quoted function bodies and comments.

Once a migration has been applied anywhere that matters, **do not edit it**. Its checksum is recorded, and a changed script is drift (see below). Write a new migration instead.

## The four operations

| Operation | On an explicit client | On the environment's `SqlClientService` | CLI |
|---|---|---|---|
| apply every pending migration, ascending | `run_on(client)` → applied versions | `run()` | `id-effect migrate run` |
| list every migration with its state | `status_on(client)` → `Vec<MigrationStatus>` | `status()` | `id-effect migrate status` |
| undo the last `n` applied migrations | `revert_on(client, n)` → reverted versions | `revert(n)` | `id-effect migrate revert --count N` |
| fail unless up to date | `check_on(client)` | `check()` | `id-effect migrate check` |

Each returns an `Effect<_, MigrateError>`. On `TestSqlClient`, script the advisory lock as free (an unscripted query returns no rows, which reads as "taken"), and every operation runs against an empty tracking table:

```rust
use std::sync::Arc;

use id_effect::{Cause, Effect, Exit};
use id_effect_sql::migrate::{MigrateError, MigrationState, Migrator};
use id_effect_sql::{SqlClient, SqlRow, TestSqlClient};

fn exit<A: Send + 'static>(eff: Effect<A, MigrateError>) -> Result<A, MigrateError> {
    match eff.block_on() {
        Exit::Success(a) => Ok(a),
        Exit::Failure(Cause::Fail(e)) => Err(e),
        Exit::Failure(cause) => panic!("not a typed failure: {cause:?}"),
    }
}

let migrator = Migrator::from_files([
    ("0001_init.sql", "CREATE TABLE todos (id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL)"),
    ("0002_add_tags.up.sql", "ALTER TABLE todos ADD COLUMN tags TEXT NOT NULL DEFAULT ''; CREATE INDEX todos_tags ON todos (tags)"),
    ("0002_add_tags.down.sql", "DROP INDEX todos_tags; ALTER TABLE todos DROP COLUMN tags"),
])?;

// An empty database whose migration lock is free.
let test = TestSqlClient::new();
test.script_query(
    "SELECT pg_try_advisory_xact_lock($1) AS locked",
    vec![SqlRow::from_pairs([("locked", true)])],
);
exit(test.connect().map_err(MigrateError::from))?;
let db: Arc<dyn SqlClient> = Arc::new(test.clone());

let states: Vec<(i64, MigrationState)> = exit(migrator.status_on(Arc::clone(&db)))?
    .iter()
    .map(|s| (s.version, s.state))
    .collect();
assert_eq!(states, [(1, MigrationState::Pending), (2, MigrationState::Pending)]);
assert_eq!(exit(migrator.check_on(Arc::clone(&db))), Err(MigrateError::Pending { versions: vec![1, 2] }));

let commits = test.commits();
assert_eq!(exit(migrator.run_on(Arc::clone(&db))), Ok(vec![1, 2]));
// The tracking table, then one transaction per migration.
assert_eq!(test.commits() - commits, 3);
let executed: Vec<String> = test.recorded_executes().into_iter().map(|(sql, _)| sql).collect();
assert!(executed.contains(&"CREATE INDEX todos_tags ON todos (tags)".to_owned())); // split per statement
let recorded = executed.iter().filter(|sql| sql.starts_with("INSERT INTO _id_effect_migrations")).count();
assert_eq!(recorded, 2);
# Ok::<(), MigrateError>(())
```

`MigrationStatus` carries the version, description, `state`, the source's `checksum` (`checksum_hex()`), the recorded `applied_checksum`, `applied_at` (a `jiff::Timestamp`) and whether the migration is `reversible`. The states are:

| `MigrationState` | Meaning |
|---|---|
| `Pending` | in the source, not applied |
| `Applied` | applied, and the script is unchanged |
| `Drifted` | applied, but the script changed since (checksum mismatch) |
| `Missing` | applied, but no longer in the source |

## Applying at startup: the layer

A service usually migrates before it serves. `Migrator::layer::<E>()` (or `migrate::layer::<E>("migrations")`, which reads the directory when the layer is built) is a layer that needs the `SqlClientService`, runs `run()` when built and provides `MigrationsApplied(Vec<i64>)`, the versions this start applied. A failure fails the build with `E::from(MigrateError)`, so the service never starts on a half-migrated schema. Put it over the client layer with `provide_merge`, which keeps the client in the output:

```rust
use id_effect::{Effect, Exit, need};
use id_effect_sql::migrate::{MigrateError, MigratedEnv, MigrationsApplied, Migrator};
use id_effect_sql::{SqlError, SqlRow, TestSqlClient, sql_client_layer};

#[derive(Debug)]
enum StartupError {
    Sql(SqlError),
    Migrate(MigrateError),
}
impl From<SqlError> for StartupError {
    fn from(e: SqlError) -> Self { StartupError::Sql(e) }
}
impl From<MigrateError> for StartupError {
    fn from(e: MigrateError) -> Self { StartupError::Migrate(e) }
}

let migrator = Migrator::from_files([("0001_init.sql", "CREATE TABLE todos (id BIGINT PRIMARY KEY)")])?;
let test = TestSqlClient::new();
test.script_query(
    "SELECT pg_try_advisory_xact_lock($1) AS locked",
    vec![SqlRow::from_pairs([("locked", true)])],
);

// Connect, then migrate; the program sees the client and `MigrationsApplied`.
let app = migrator.layer::<StartupError>().provide_merge(sql_client_layer::<StartupError>(test.clone()));
let program: Effect<Vec<i64>, StartupError, MigratedEnv> =
    need::<MigrationsApplied>().map(|applied| applied.0.clone());
assert!(matches!(program.provide_layer(app).block_on(), Exit::Success(ref v) if v == &[1]));
# Ok::<(), MigrateError>(())
```

In production the client layer is `PgSqlClient::layer(PgPoolConfig::from_url(url))`, run on `TokioRuntime`:

```rust,no_run
use id_effect::{Effect, need};
use id_effect_sql::migrate::{self, MigrateError, MigratedEnv, MigrationsApplied};
use id_effect_sql::pg::{PgPoolConfig, PgSqlClient};
use id_effect_sql::SqlError;
use id_effect_tokio::TokioRuntime;

# #[derive(Debug)]
# enum StartupError { Sql(SqlError), Migrate(MigrateError) }
# impl From<SqlError> for StartupError { fn from(e: SqlError) -> Self { StartupError::Sql(e) } }
# impl From<MigrateError> for StartupError { fn from(e: MigrateError) -> Self { StartupError::Migrate(e) } }
# async fn start(url: String) -> std::io::Result<()> {
let program: Effect<usize, StartupError, MigratedEnv> =
    need::<MigrationsApplied>().map(|applied| applied.0.len());
let app = migrate::layer::<StartupError>("migrations")              // migrate on build
    .provide_merge(PgSqlClient::layer(PgPoolConfig::from_url(url))); // open the pool first
let exit = TokioRuntime::current()?.run(program.provide_layer(app)).await;
# let _ = exit;
# Ok(())
# }
# fn main() {}
```

`migrate::layer("migrations")` resolves the directory against the working directory of the process, so a container image must ship it. To make the binary self-contained, embed the scripts at compile time instead:

- with sqlx's macro: add `sqlx = { version = "0.8", default-features = false, features = ["macros", "migrate"] }` to the binary, declare `static MIGRATIONS: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");` and build the layer from `Migrator::from_sqlx(&MIGRATIONS)?.layer()`. Checksums are the same as from the directory, so a database migrated by the CLI is up to date for the binary.
- without it: `Migrator::from_files([("0001_init.sql", include_str!("../migrations/0001_init.sql")), …])`.

Startup migration and the CLI are two ways to drive one engine; pick one per environment. Running `id-effect migrate run` as a deploy step keeps schema changes out of the service's start-up path and its permissions; the layer is simpler for small services and tests.

## Concurrency: several replicas starting at once

Three replicas of a service that each migrate on start race for the same database. The engine makes that safe:

- every migration runs in **its own transaction** together with its tracking-table row, so a migration is either applied and recorded or neither. A failing statement rolls that migration back and stops the run (`MigrateError::Failed` names the file and carries the database error); migrations applied before it stay applied;
- each of those transactions first takes the transaction-scoped **advisory lock** (`pg_try_advisory_xact_lock`, key `migrate::DEFAULT_LOCK_KEY`), polling until the lock timeout, then re-reads whether its version is applied. The replica that gets the lock applies the migration; the others find it recorded and skip it. Each migration is applied at most once;
- a migrator that cannot get the lock in time fails with `MigrateError::LockTimeout`; raise it with `with_lock_timeout` or `--lock-timeout SECS` for long migrations.

The lock is held per migration, not for the whole run, so a long data migration blocks other migrators only while it runs.

A script that starts with `-- no-transaction` is rejected (`MigrateError::Source`): statements that cannot run inside a transaction, such as `CREATE INDEX CONCURRENTLY`, need to be run outside the engine.

## Drift, `check` and CI

When an applied migration's script changed (`Drifted`) or disappeared from the source (`Missing`), `run`, `revert` and `check` fail with `MigrateError::ChecksumMismatch` / `MigrateError::Missing` **before applying anything**. Drift means the database's schema no longer follows from the repository; fix it by restoring the applied script and writing a new migration.

`check` is the CI gate: it succeeds only when every migration is applied and unchanged, and fails with `Pending`, `ChecksumMismatch` or `Missing` otherwise; `id-effect migrate check` exits `1` then. A GitHub Actions job that migrates a scratch database and then checks it:

```yaml
jobs:
  migrations:
    runs-on: ubuntu-latest
    services:
      postgres:
        image: postgres:17
        env:
          POSTGRES_PASSWORD: postgres
        ports: ["5432:5432"]
        options: >-
          --health-cmd pg_isready --health-interval 5s --health-timeout 5s --health-retries 10
    env:
      DATABASE_URL: postgres://postgres:postgres@localhost:5432/postgres
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo install id_effect_tool --locked
      - run: id-effect migrate run     # every script applies cleanly, in order
      - run: id-effect migrate check   # and the result is up to date
```

The same `id-effect migrate check --database-url "$STAGING_DATABASE_URL"` before a deploy tells you whether staging has drifted or is behind.

## Status and `--json`

`id-effect migrate status` prints one line per migration (state, version, when it was applied, description) and the counts; it always exits `0`:

```text
id-effect migrate status: migrations (table _id_effect_migrations)
  applied     1  2026-10-16 21:09:02Z  init
  applied     2  2026-10-16 21:09:02Z  add tags  (reversible)
  pending     3                        add users

2 applied, 1 pending, 0 drifted, 0 missing
```

Every command takes `--json` and then prints one object on stdout: `{"ok": true, "applied": [3]}` for `run`, `{"ok": true, "reverted": [3]}` for `revert`, `{"ok": true, "migrations": 3}` for `check`, and for `status` `{"ok", "dir", "table", "counts", "migrations": [{"version", "description", "state", "reversible", "checksum", "applied_checksum", "applied_at"}]}`. A failure is `{"ok": false, "error": {"kind", "message", "version"?, "file"?, "versions"?}}`. The database URL comes from `--database-url`, else `DATABASE_URL`, else `./.env`, and is never printed. Exit codes: `0` done, `1` the migrations need attention (pending for `check`, drift, a failing script, no down script), `2` usage (bad arguments, no URL), `3` the database (cannot connect, lock timeout, bookkeeping failed).

## Reverting

`revert_on(client, n)` (`id-effect migrate revert --count N`, default 1) runs the down scripts of the last `n` applied migrations, newest first, each in its own transaction under the lock, and deletes their tracking rows; it returns the reverted versions. If one of them has no `.down.sql` it fails with `MigrateError::MissingDown` before reverting anything. Reverting is a development and incident tool: a down script that drops a column drops its data too, and in production a new forward migration is usually the safer undo.

## Errors

`MigrateError` is `#[non_exhaustive]`; match it with a wildcard arm. `Failed` and `Sql` chain the `SqlError` through `std::error::Error::source`:

```rust
use id_effect_sql::SqlError;
use id_effect_sql::migrate::MigrateError;

/// What an operator should do about a failed migration command.
fn advice(e: &MigrateError) -> &'static str {
    match e {
        MigrateError::Pending { .. } => "run the pending migrations",
        MigrateError::ChecksumMismatch { .. } | MigrateError::Missing { .. } => {
            "restore the applied script and write a new migration"
        }
        MigrateError::Failed { .. } => "fix the script: it was rolled back and the run stopped",
        MigrateError::MissingDown { .. } => "add a .down.sql or undo it with a new migration",
        MigrateError::LockTimeout { .. } => "another migrator holds the lock: wait or raise the timeout",
        MigrateError::Source(_) => "fix the migration files",
        MigrateError::Sql(_) => "check the database connection and permissions",
        _ => "see the message",
    }
}

let drift = MigrateError::ChecksumMismatch { version: 2, file: "0002_add_tags.up.sql".into() };
assert_eq!(
    drift.to_string(),
    "migration 2 (0002_add_tags.up.sql) changed after it was applied: checksum mismatch"
);
assert_eq!(advice(&drift), "restore the applied script and write a new migration");

let failed = MigrateError::Failed {
    version: 3,
    file: "0003_add_users.sql".into(),
    error: SqlError::QueryFailed { sql: String::new(), message: "relation \"users\" already exists".into() },
};
assert!(std::error::Error::source(&failed).is_some());
assert_eq!(MigrateError::Pending { versions: vec![3, 4] }.to_string(), "2 pending migration(s): 3, 4");
```

| Variant | When |
|---|---|
| `Source(message)` | unreadable directory, bad file name, duplicate version, down script without up script, `-- no-transaction` |
| `Pending { versions }` | `check`: migrations not applied yet |
| `ChecksumMismatch { version, file }` | an applied script changed |
| `Missing { version }` | an applied migration is not in the source |
| `Failed { version, file, error }` | a statement failed; that migration was rolled back |
| `LockTimeout { key, timeout }` | another migrator held the lock for the whole timeout |
| `MissingDown { version, file }` | `revert`: no down script |
| `Sql(error)` | bookkeeping failed (connection, tracking table, lock query) |

## Try it

[`crates/id_effect_sql/examples/030_migrate.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_sql/examples/030_migrate.rs) runs `status`, `check`, `run` and `revert` on the in-memory client:

```sh
cargo run -p id_effect_sql --example 030_migrate --features migrate,pg
DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres \
  cargo run -p id_effect_sql --example 030_migrate --features migrate,pg -- --live
```

With `DATABASE_URL` set (or `--live`) it runs the same steps on PostgreSQL and reverts them at the end. The live behaviour (concurrent runners applying once, rollback of a failing migration, revert, drift, lock timeout) is tested in [`crates/id_effect_sql/tests/migrate.rs`](https://github.com/Industrial/id_effect/blob/main/crates/id_effect_sql/tests/migrate.rs).
