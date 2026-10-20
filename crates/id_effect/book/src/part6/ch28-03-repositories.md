# Query builder, entities and repositories

[Typed SQL](ch28-01-typed-sql.md) covers statements written as strings. Two features of `id_effect_sql` build them for you:

- **`query`**: statements built with [sea-query] and run through the `SqlClient` in the environment, rows decoded with `FromRow`.
- **`repository`** (includes `query`): `#[derive(Entity)]` maps a struct to a table, and `Repository<T>` runs its CRUD statements with optimistic locking and global filters (soft delete, tenant, custom scopes).

```toml
id_effect_sql = { version = "0.11", features = ["pg", "repository"] }
```

## The query builder (feature `query`)

`id_effect_sql::query_builder::sea_query` is a re-export, so your builder and the crate use the same sea-query version. `stmt.build_sql(Dialect::Postgres)` returns the SQL (`$1, $2, …` on PostgreSQL, `?` on MySQL and SQLite) and its parameters as `SqlValue`s. `fetch_all_as::<Todo, _, _>(Dialect::Postgres, &stmt)` runs the statement on the `SqlClient` in the environment and decodes every row with `FromRow`, exactly like `query_as` does for raw SQL. `fetch_one_as` (`SqlError::RowNotFound` without a row), `fetch_optional_as` and `execute_query` (the affected row count) cover the other cases.

Plain values go into a builder as themselves (`Expr::val(7)`, strings, jiff dates). Any `ToSqlValue` type goes in as `Expr::val(x.to_sql_value())`:

```rust
use id_effect::Exit;
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait, Order, Query};
use id_effect_sql::query_builder::{BuildSql, Dialect, fetch_all_as};
use id_effect_sql::uuid::Uuid;
use id_effect_sql::{FromRow, SqlClientEnv, SqlError, SqlRow, SqlValue, TestSqlClient, ToSqlValue, sql_client_layer};

#[derive(Debug, PartialEq, FromRow)]
struct Todo { id: i64, title: String }

let owner = Uuid::nil();
let open = Query::select()
    .columns(["id", "title"])
    .from("todos")
    .and_where(Expr::col("done").eq(false))
    .and_where(Expr::col("owner").eq(Expr::val(owner.to_sql_value())))
    .order_by("id", Order::Asc)
    .limit(10)
    .to_owned();

let (sql, params) = open.build_sql(Dialect::Postgres)?;
assert_eq!(
    sql,
    r#"SELECT "id", "title" FROM "todos" WHERE "done" = $1 AND "owner" = $2 ORDER BY "id" ASC LIMIT $3"#
);
assert_eq!(params, [SqlValue::Bool(false), SqlValue::Uuid(owner), SqlValue::I64(10)]);
let (sqlite, _) = open.build_sql(Dialect::Sqlite)?;
assert!(sqlite.contains(r#"WHERE "done" = ? AND "owner" = ?"#));

let test = TestSqlClient::new();
test.script_query(sql, vec![SqlRow::from_pairs([("id", SqlValue::I64(1)), ("title", "docs".into())])]);
let todos = fetch_all_as::<Todo, SqlClientEnv, _>(Dialect::Postgres, &open)
    .provide_layer(sql_client_layer::<SqlError>(test));
assert!(matches!(todos.block_on(), Exit::Success(v) if v == [Todo { id: 1, title: "docs".into() }]));
# Ok::<(), SqlError>(())
```

The statement is rendered when `fetch_*` is called. A parameter the bridge cannot carry fails the effect with an error naming the parameter (`$2`), and no query runs.

### Joins, ordering and paging

Columns of a join are `(table, column)` pairs; `inner_join` / `left_join` take the joined table and the `ON` condition (`Expr::col(a).equals(b)` compares two columns), and `expr_as` names a selected expression so `FromRow` finds it. `order_by` is repeatable — the first call is the primary sort key. For paging, the `Page` of the repository feature (below) carries `limit` and `offset`; `Page::number(n, size)` is the 0-based page `n`:

```rust
use id_effect::Exit;
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use id_effect_sql::query_builder::{BuildSql, Dialect, fetch_all_as};
use id_effect_sql::{FromRow, Page, SqlClientEnv, SqlError, SqlRow, SqlValue, TestSqlClient, sql_client_layer};

/// An order with its customer's name.
#[derive(Debug, PartialEq, FromRow)]
struct OrderLine { id: i64, customer: String, total: i64 }

/// Orders of active customers worth at least `min_total`, the largest first (ties by id).
fn big_orders(min_total: i64, page: Page) -> SelectStatement {
    Query::select()
        .column(("orders", "id"))
        .expr_as(Expr::col(("customers", "name")), "customer")
        .column(("orders", "total"))
        .from("orders")
        .inner_join("customers", Expr::col(("orders", "customer_id")).equals(("customers", "id")))
        .and_where(Expr::col(("orders", "total")).gte(min_total))
        .and_where(Expr::col(("customers", "active")).eq(true))
        .order_by(("orders", "total"), Order::Desc)
        .order_by(("orders", "id"), Order::Asc)
        .limit(page.limit)
        .offset(page.offset)
        .to_owned()
}

let second_page = big_orders(100, Page::number(1, 20));
let (sql, params) = second_page.build_sql(Dialect::Postgres)?;
assert_eq!(
    sql,
    r#"SELECT "orders"."id", "customers"."name" AS "customer", "orders"."total" FROM "orders" INNER JOIN "customers" ON "orders"."customer_id" = "customers"."id" WHERE "orders"."total" >= $1 AND "customers"."active" = $2 ORDER BY "orders"."total" DESC, "orders"."id" ASC LIMIT $3 OFFSET $4"#
);
assert_eq!(params, [SqlValue::I64(100), SqlValue::Bool(true), SqlValue::I64(20), SqlValue::I64(20)]);

let test = TestSqlClient::new();
test.script_query(sql, vec![SqlRow::from_pairs([
    ("id", SqlValue::I64(21)), ("customer", "ada".into()), ("total", SqlValue::I64(250)),
])]);
let lines = fetch_all_as::<OrderLine, SqlClientEnv, _>(Dialect::Postgres, &second_page)
    .provide_layer(sql_client_layer::<SqlError>(test));
assert!(matches!(
    lines.block_on(),
    Exit::Success(v) if v == [OrderLine { id: 21, customer: "ada".into(), total: 250 }]
));
# Ok::<(), SqlError>(())
```

`left_join` keeps the rows without a match (their joined columns are NULL: decode them as `Option<T>`), and `group_by_col` with an aggregate (`Expr::col(..).count()`) summarises them. `crates/id_effect_sql/tests/query.rs` (`joins_render_with_ordering_and_paging`, `joins_run_on_sqlite`) renders an inner and a left join and runs them, page by page, on SQLite. The repository's own `find_page` (below) pages one entity's table in primary-key order; reach for the query builder when a read spans tables.

## Entities: `#[derive(Entity)]`

`#[derive(FromRow, Entity)]` maps a struct to a table:

- the table is `#[entity(table = "..")]`, by default the type name in snake case;
- the columns are the `FromRow` columns (`#[from_row(rename = "..")]` renames one);
- `#[entity(id)]` marks the primary key; add `generated` when the database assigns it, and inserts then leave it out and return the stored row;
- `#[entity(version)]` marks an integer column used for optimistic locking.

The derive generates the `Entity` trait: the mapping as constants, and `values()`, the column values an insert or update writes.

```rust
use id_effect_sql::{Entity, FromRow, SqlValue};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "accounts")]
struct Account {
    #[entity(id, generated)]
    id: i64,
    #[from_row(rename = "display_name")]
    name: String,
    #[entity(version)]
    version: i64,
}

assert_eq!(Account::TABLE, "accounts");
assert_eq!(Account::COLUMNS, ["id", "display_name", "version"]);
assert_eq!((Account::ID, Account::ID_GENERATED, Account::VERSION), ("id", true, Some("version")));

let a = Account { id: 7, name: "ada".into(), version: 1 };
assert_eq!(a.values()[1], ("display_name", SqlValue::Text("ada".into())));
```

A non-integer `version` field or an entity without `#[entity(id)]` is a compile error.

## `Repository<T>`

`Repository::<Account>::new(Dialect::Postgres)` gives `find`, `find_all`, `find_by` (any sea-query condition), `find_page`, `count`, `count_by`, `exists`, `insert`, `update`, `delete` and `hard_delete`. Each is an effect over the `SqlClientService` in its environment, so a repository is a plain value you can keep in your router state or build per request. Tests can run it on `TestSqlClient` and check the statements it sends:

```rust
use id_effect::{Cause, Effect, Exit};
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait};
use id_effect_sql::{Dialect, Entity, FromRow, Page, Repository, SqlClient, SqlClientEnv, SqlError, SqlRow, SqlValue, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "accounts")]
struct Account {
    #[entity(id, generated)]
    id: i64,
    owner: String,
    balance: i64,
}

# fn run<A: Send + 'static>(test: &TestSqlClient, eff: Effect<A, SqlError, SqlClientEnv>) -> Result<A, SqlError> {
#     match eff.provide_all(sql_client_env(test.clone())).block_on() {
#         Exit::Success(a) => Ok(a),
#         Exit::Failure(Cause::Fail(e)) => Err(e),
#         Exit::Failure(c) => panic!("{c:?}"),
#     }
# }
let test = TestSqlClient::new();
let _ = test.connect().block_on();
# test.script_query(
#     r#"INSERT INTO "accounts" ("owner", "balance") VALUES ($1, $2) RETURNING "id", "owner", "balance""#,
#     vec![SqlRow::from_pairs([("id", SqlValue::I64(1)), ("owner", "ada".into()), ("balance", SqlValue::I64(100))])],
# );
let accounts = Repository::<Account>::new(Dialect::Postgres);

// The database assigns the id; the stored row comes back.
let ada = run(&test, accounts.insert(Account { id: 0, owner: "ada".into(), balance: 100 }))?;
assert_eq!(ada.id, 1);

let _rich = run(&test, accounts.find_by(Expr::col("balance").gte(50)))?;
let _page = run(&test, accounts.find_page(Expr::col("owner").eq("ada"), Page::number(2, 20)))?;
let _gone = run(&test, accounts.find(42))?; // Option<Account>

let sent: Vec<String> = test.recorded_queries().into_iter().map(|(sql, _)| sql).collect();
assert_eq!(
    sent[1..],
    [
        r#"SELECT "id", "owner", "balance" FROM "accounts" WHERE "balance" >= $1 ORDER BY "id" ASC"#,
        r#"SELECT "id", "owner", "balance" FROM "accounts" WHERE "owner" = $1 ORDER BY "id" ASC LIMIT $2 OFFSET $3"#,
        r#"SELECT "id", "owner", "balance" FROM "accounts" WHERE "id" = $1 LIMIT $2"#,
    ]
);
# Ok::<(), SqlError>(())
```

Results come back in primary-key order. `Page::number(2, 20)` is the third page of 20 rows (`Page::new(limit, offset)` for raw values).

### Inside a transaction

`repo.on(executor)` binds the same operations to an executor: an open transaction (`Arc<dyn SqlTransaction>` or `SqlTransactionService`) or a client. The bound operations are environment-free effects; inside `transaction(|tx| ..)` over a larger environment, `.narrow()` them into it.

```rust
use std::sync::Arc;

use id_effect::{Exit, succeed};
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlRow, SqlTransaction, TestSqlClient, with_transaction};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "accounts")]
struct Account {
    #[entity(id)]
    id: i64,
    owner: String,
}

let test = TestSqlClient::new();
let _ = test.connect().block_on();
# test.script_query(r#"SELECT "id" FROM "accounts" WHERE "id" = $1 LIMIT $2"#, vec![SqlRow::from_pairs([("id", 1i64)])]);
# test.script_execute(r#"DELETE FROM "accounts" WHERE "id" = $1"#, 1);
let repo = Repository::<Account>::new(Dialect::Postgres);
let client: Arc<dyn SqlClient> = Arc::new(test.clone());

let deleted = with_transaction(client, move |tx: Arc<dyn SqlTransaction>| {
    let accounts = repo.on(tx);
    accounts.exists(1).flat_map(move |found| if found { accounts.delete(1) } else { succeed(false) })
});
assert!(matches!(deleted.block_on(), Exit::Success(true)));
assert_eq!(test.commits(), 1);
```

## Optimistic locking

An `#[entity(version)]` column is checked and incremented on every update: `UPDATE … SET …, version = version + 1 WHERE id = $n AND version = $m`. The returned entity carries the new version. When another writer updated the row since you read it, the update matches no row and fails with `SqlError::StaleVersion { entity, id }`; reload and retry (or report a conflict, HTTP 409). A row that is gone, or hidden by a filter, is `SqlError::RowNotFound` instead.

```rust
use id_effect::{Cause, Exit};
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlError, SqlRow, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "accounts")]
struct Account {
    #[entity(id)]
    id: i64,
    owner: String,
    #[entity(version)]
    version: i64,
}

let test = TestSqlClient::new();
let _ = test.connect().block_on();
let update = r#"UPDATE "accounts" SET "owner" = $1, "version" = "version" + $2 WHERE "id" = $3 AND "version" = $4"#;
let repo = Repository::<Account>::new(Dialect::Postgres);
let read = Account { id: 7, owner: "ada".into(), version: 2 };

// The row still has version 2: the update wins and returns version 3.
test.script_execute(update, 1);
let saved = repo.update::<SqlClientEnv, _>(read.clone()).provide_all(sql_client_env(test.clone()));
assert!(matches!(saved.block_on(), Exit::Success(a) if a.version == 3));

// Someone else got there first: no row matched, but the row exists.
test.script_execute(update, 0);
test.script_query(r#"SELECT "id" FROM "accounts" WHERE "id" = $1 LIMIT $2"#, vec![SqlRow::from_pairs([("id", 7i64)])]);
let lost = repo.update::<SqlClientEnv, _>(read).provide_all(sql_client_env(test.clone()));
let Exit::Failure(Cause::Fail(err)) = lost.block_on() else { panic!("expected a stale version") };
assert_eq!(err, SqlError::StaleVersion { entity: "Account".into(), id: "7".into() });
assert_eq!(err.to_string(), "sql stale version: Account 7 was changed by another writer");
```

A soft delete (next section) also increments the version.

## Global filters

Filters declared on the entity are AND-ed into every `SELECT`, `UPDATE` and `DELETE` the repository builds:

| Attribute | Effect |
|---|---|
| `soft_delete = "deleted_at"` | rows whose column is not NULL are hidden; `delete` sets it to the current time (`hard_delete` really deletes) |
| `scope = path::to::fn` | a fixed condition, from a `fn() -> impl Into<Condition>` (repeatable) |
| `tenant = "org_id"` | only the repository's tenant; see below |

The columns need not be fields. Two methods lift filters for one repository value: `with_deleted()` lifts only the soft-delete filter, `unscoped()` lifts all of them (admin tools, data migrations). Both are visible in the code that uses them.

```rust
use id_effect::Exit;
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait};
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, TestSqlClient, sql_client_env};

fn not_archived() -> Expr {
    Expr::col("archived").eq(false)
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "notes", soft_delete = "deleted_at", scope = not_archived)]
struct Note {
    #[entity(id)]
    id: i64,
    title: String,
}

let test = TestSqlClient::new();
let _ = test.connect().block_on();
let notes = Repository::<Note>::new(Dialect::Postgres);
for repo in [notes.clone(), notes.with_deleted(), notes.unscoped()] {
    let _ = repo.find_all::<SqlClientEnv, _>().provide_all(sql_client_env(test.clone())).block_on();
}
let _ = notes.delete::<SqlClientEnv, _>(1).provide_all(sql_client_env(test.clone())).block_on();

let selects: Vec<String> = test.recorded_queries().into_iter().map(|(sql, _)| sql).collect();
assert_eq!(
    selects,
    [
        r#"SELECT "id", "title" FROM "notes" WHERE "deleted_at" IS NULL AND "archived" = $1 ORDER BY "id" ASC"#,
        r#"SELECT "id", "title" FROM "notes" WHERE "archived" = $1 ORDER BY "id" ASC"#,
        r#"SELECT "id", "title" FROM "notes" ORDER BY "id" ASC"#,
    ]
);
assert_eq!(
    test.recorded_executes()[0].0,
    r#"UPDATE "notes" SET "deleted_at" = $1 WHERE "deleted_at" IS NULL AND "archived" = $2 AND "id" = $3"#
);
```

Statements you build yourself with the query builder, and raw SQL, are not filtered.

### Tenant scoping

`tenant = "org_id"` filters every statement by the repository's tenant. It comes from `with_tenant(v)`, or from the `CurrentTenant` capability with `Repository::for_current_tenant(dialect)`. Inserts write the tenant column (whatever the field says) and updates never change it. With no tenant set, every call fails with `SqlError::Unsupported` instead of returning unscoped data:

```rust
use id_effect::{Cause, Exit};
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlClientEnv, SqlError, SqlValue, TestSqlClient, sql_client_env};

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "invoices", tenant = "org_id")]
struct Invoice {
    #[entity(id)]
    id: i64,
    amount: i64,
}

let test = TestSqlClient::new();
let _ = test.connect().block_on();
let invoices = Repository::<Invoice>::new(Dialect::Postgres);

let no_tenant = invoices.find_all::<SqlClientEnv, _>().provide_all(sql_client_env(test.clone()));
assert!(matches!(
    no_tenant.block_on(),
    Exit::Failure(Cause::Fail(SqlError::Unsupported(m))) if m.contains("tenant column `org_id`")
));
assert!(test.recorded_queries().is_empty(), "nothing was sent");

let acme = invoices.with_tenant("acme");
let _ = acme.find_all::<SqlClientEnv, _>().provide_all(sql_client_env(test.clone())).block_on();
assert_eq!(
    test.recorded_queries()[0],
    (
        r#"SELECT "id", "amount" FROM "invoices" WHERE "org_id" = $1 ORDER BY "id" ASC"#.to_owned(),
        vec![SqlValue::Text("acme".into())]
    )
);
```

[Tenancy and the audit log](ch30-07-audit-tenancy.md) shows the whole request path: from the authenticated principal to `CurrentTenant`, the filtered repository and an audit record in the same transaction.

## Dialects

The repository renders each statement for its `Dialect`. PostgreSQL and SQLite insert with `RETURNING`. MySQL has no `RETURNING`, so an insert is followed by a read of the row by id, and a `generated` id is `SqlError::Unsupported` there (assign ids in the application, e.g. UUIDs).

## Tests and example

- `crates/id_effect_sql/tests/repository.rs` checks the statements of every dialect over `TestSqlClient`, then runs one suite on SQLite in memory and on PostgreSQL (`DATABASE_URL`): `stale_version_is_rejected`, `global_filter_hides_soft_deleted` and `tenant_filter_scopes_every_query`. `tests/query.rs` covers the query builder and its value bridge.
- `crates/id_effect_sql/examples/040_repository.rs` runs CRUD, paging, a lost update, a transfer in one transaction, soft delete, a custom scope and a sea-query report on SQLite in memory: `cargo run -p id_effect_sql --example 040_repository --features repository,sqlite`.

[sea-query]: https://docs.rs/sea-query
