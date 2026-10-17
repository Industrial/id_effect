//! sea-query statements (feature `query`): the crate-root API, and one live PostgreSQL run that
//! builds, executes and decodes `INSERT … RETURNING` / `SELECT` / `UPDATE` / `DELETE`. Joins
//! with ordering and paging are rendered for every dialect and run on SQLite in memory (feature
//! `sqlite`).
//!
//! The live test needs a database at `DATABASE_URL`; without one it prints `SKIP`, and
//! `ID_EFFECT_REQUIRE_PG=1` turns that into a failure (the `tests/pg.rs` policy).

use id_effect::Exit;
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use id_effect_sql::query_builder::sql_values;
use id_effect_sql::{
  BuildSql, Dialect, FromRow, SqlClientEnv, SqlError, SqlRow, SqlValue, TestSqlClient, ToSqlValue,
  fetch_all_as, fetch_one_as, sql_client_layer,
};
use jiff::Timestamp;
use rust_decimal::Decimal;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, FromRow)]
struct Item {
  id: i64,
  key: Uuid,
  title: String,
  due: jiff::civil::Date,
  at: Timestamp,
  price: Decimal,
  meta: serde_json::Value,
  note: Option<String>,
}

fn item(id: i64) -> Item {
  Item {
    id,
    key: Uuid::from_u128(0xfeed),
    title: "docs".into(),
    due: jiff::civil::date(2024, 2, 29),
    at: "2024-02-29T12:34:56.789012Z".parse().unwrap(),
    price: Decimal::new(1999, 2),
    meta: serde_json::json!({"tags": ["a", "b"]}),
    note: None,
  }
}

/// `INSERT … RETURNING *` built from an item's typed values (`ToSqlValue` into the builder).
fn insert(it: &Item) -> impl BuildSql {
  Query::insert()
    .into_table("query_items")
    .columns(["key", "title", "due", "at", "price", "meta", "note"])
    .values_panic([
      it.key.to_sql_value().into(),
      it.title.to_sql_value().into(),
      it.due.to_sql_value().into(),
      it.at.to_sql_value().into(),
      it.price.to_sql_value().into(),
      it.meta.to_sql_value().into(),
      it.note.to_sql_value().into(),
    ])
    .returning_all()
    .to_owned()
}

fn by_title(title: &str) -> impl BuildSql {
  Query::select()
    .columns(["id", "key", "title", "due", "at", "price", "meta", "note"])
    .from("query_items")
    .and_where(Expr::col("title").eq(title))
    .order_by("id", Order::Asc)
    .to_owned()
}

#[test]
fn crate_root_api_renders_and_runs_statements() {
  let it = item(1);
  let (sql, params) = insert(&it).build_sql(Dialect::Postgres).unwrap();
  assert_eq!(
    sql,
    r#"INSERT INTO "query_items" ("key", "title", "due", "at", "price", "meta", "note") VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"#
  );
  assert_eq!(
    params,
    [
      SqlValue::Uuid(it.key),
      SqlValue::Text("docs".into()),
      SqlValue::Date(it.due),
      SqlValue::Timestamp(it.at),
      SqlValue::Decimal(it.price),
      SqlValue::Json(it.meta.clone()),
      SqlValue::Null,
    ]
  );
  let (sqlite, _) = insert(&it).build_sql(Dialect::Sqlite).unwrap();
  assert!(sqlite.ends_with("VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING *"));
  assert_eq!(
    sql_values(id_effect_sql::query_builder::sea_query::Values(vec![
      7i32.into()
    ])),
    Ok(vec![SqlValue::I32(7)])
  );

  let test = TestSqlClient::new();
  let row = SqlRow::from_pairs([
    ("id", SqlValue::I64(1)),
    ("key", it.key.to_sql_value()),
    ("title", "docs".into()),
    ("due", it.due.to_sql_value()),
    ("at", it.at.to_sql_value()),
    ("price", it.price.to_sql_value()),
    ("meta", it.meta.to_sql_value()),
    ("note", SqlValue::Null),
  ]);
  test.script_query(sql, vec![row.clone()]);
  let (select_sql, _) = by_title("docs").build_sql(Dialect::Postgres).unwrap();
  test.script_query(select_sql, vec![row]);
  let run = |eff: id_effect::Effect<Vec<Item>, SqlError, SqlClientEnv>| {
    eff
      .provide_layer(sql_client_layer::<SqlError>(test.clone()))
      .block_on()
  };
  let inserted = run(fetch_one_as::<Item, _, _>(Dialect::Postgres, &insert(&it)).map(|i| vec![i]));
  assert!(matches!(inserted, Exit::Success(ref v) if v == &[item(1)]));
  let found = run(fetch_all_as::<Item, _, _>(
    Dialect::Postgres,
    &by_title("docs"),
  ));
  assert!(matches!(found, Exit::Success(ref v) if v == &[item(1)]));
}

/// An order with its customer's name, read through a join.
#[derive(Debug, Clone, PartialEq, FromRow)]
struct OrderLine {
  id: i64,
  customer: String,
  total: i64,
}

/// Orders of active customers worth at least `min_total`: an inner join, a filter on each table,
/// the largest first (ties by id), one page of `limit` rows after `offset`.
fn big_orders(min_total: i64, limit: u64, offset: u64) -> SelectStatement {
  Query::select()
    .column(("orders", "id"))
    .expr_as(Expr::col(("customers", "name")), "customer")
    .column(("orders", "total"))
    .from("orders")
    .inner_join(
      "customers",
      Expr::col(("orders", "customer_id")).equals(("customers", "id")),
    )
    .and_where(Expr::col(("orders", "total")).gte(min_total))
    .and_where(Expr::col(("customers", "active")).eq(true))
    .order_by(("orders", "total"), Order::Desc)
    .order_by(("orders", "id"), Order::Asc)
    .limit(limit)
    .offset(offset)
    .to_owned()
}

/// Every customer with the number of its orders, customers without one included: a left join
/// and a `GROUP BY`.
fn order_counts() -> SelectStatement {
  Query::select()
    .expr_as(Expr::col(("customers", "name")), "customer")
    .expr_as(Expr::col(("orders", "id")).count(), "orders")
    .from("customers")
    .left_join(
      "orders",
      Expr::col(("orders", "customer_id")).equals(("customers", "id")),
    )
    .group_by_col(("customers", "name"))
    .order_by(("customers", "name"), Order::Asc)
    .to_owned()
}

#[test]
fn joins_render_with_ordering_and_paging() {
  let (sql, params) = big_orders(100, 2, 2).build_sql(Dialect::Postgres).unwrap();
  assert_eq!(
    sql,
    r#"SELECT "orders"."id", "customers"."name" AS "customer", "orders"."total" FROM "orders" INNER JOIN "customers" ON "orders"."customer_id" = "customers"."id" WHERE "orders"."total" >= $1 AND "customers"."active" = $2 ORDER BY "orders"."total" DESC, "orders"."id" ASC LIMIT $3 OFFSET $4"#
  );
  assert_eq!(
    params,
    [
      SqlValue::I64(100),
      SqlValue::Bool(true),
      SqlValue::I64(2),
      SqlValue::I64(2)
    ]
  );
  let (mysql, _) = big_orders(100, 2, 2).build_sql(Dialect::MySql).unwrap();
  assert!(
    mysql.contains("INNER JOIN `customers` ON `orders`.`customer_id` = `customers`.`id`"),
    "{mysql}"
  );
  let (counts, _) = order_counts().build_sql(Dialect::Sqlite).unwrap();
  assert_eq!(
    counts,
    r#"SELECT "customers"."name" AS "customer", COUNT("orders"."id") AS "orders" FROM "customers" LEFT JOIN "orders" ON "orders"."customer_id" = "customers"."id" GROUP BY "customers"."name" ORDER BY "customers"."name" ASC"#
  );

  // The joined rows decode with `FromRow` like any other.
  let test = TestSqlClient::new();
  test.script_query(
    sql,
    vec![SqlRow::from_pairs([
      ("id", SqlValue::I64(3)),
      ("customer", "ada".into()),
      ("total", SqlValue::I64(250)),
    ])],
  );
  let page = fetch_all_as::<OrderLine, SqlClientEnv, _>(Dialect::Postgres, &big_orders(100, 2, 2))
    .provide_layer(sql_client_layer::<SqlError>(test))
    .block_on();
  assert!(
    matches!(page, Exit::Success(ref v) if v == &[OrderLine { id: 3, customer: "ada".into(), total: 250 }]),
    "{page:?}"
  );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn joins_run_on_sqlite() {
  use id_effect::Effect;
  use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
  use id_effect_sql::{SqlClient, provide_sql_client};
  use id_effect_tokio::TokioRuntime;

  let pool = sqlite_pool_from_config(SqlitePoolConfig::memory().with_max_size(1))
    .await
    .expect("sqlite pool");
  let client = SqliteSqlClient::new(pool);
  let rt = TokioRuntime::current().expect("tokio");
  for sql in [
    "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
    "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL REFERENCES customers (id), total INTEGER NOT NULL)",
    "INSERT INTO customers VALUES (1, 'ada', 1), (2, 'bob', 0), (3, 'cy', 1)",
    "INSERT INTO orders VALUES (1, 1, 500), (2, 2, 900), (3, 1, 120), (4, 1, 80), (5, 1, 300), (6, 3, 300)",
  ] {
    let done = rt.run(client.execute(sql, &[])).await;
    assert!(matches!(done, Exit::Success(_)), "{sql}: {done:?}");
  }
  async fn rows<T: FromRow + Send + 'static>(
    rt: &TokioRuntime,
    client: &SqliteSqlClient,
    stmt: &SelectStatement,
  ) -> Vec<T> {
    let eff: Effect<Vec<T>, SqlError, SqlClientEnv> = fetch_all_as(Dialect::Sqlite, stmt);
    match rt
      .run(eff.provide_layer(provide_sql_client::<SqlError>(client.clone())))
      .await
    {
      Exit::Success(rows) => rows,
      Exit::Failure(c) => panic!("query failed: {c:?}"),
    }
  }
  let line = |id, customer: &str, total| OrderLine {
    id,
    customer: customer.into(),
    total,
  };

  // Active customers only (bob's 900 is left out), largest first, ties by id; pages of two.
  let first = rows::<OrderLine>(&rt, &client, &big_orders(100, 2, 0)).await;
  assert_eq!(first, [line(1, "ada", 500), line(5, "ada", 300)]);
  let second = rows::<OrderLine>(&rt, &client, &big_orders(100, 2, 2)).await;
  assert_eq!(second, [line(6, "cy", 300), line(3, "ada", 120)]);
  let past_the_end = rows::<OrderLine>(&rt, &client, &big_orders(100, 2, 4)).await;
  assert!(past_the_end.is_empty());

  let counts = rows::<(String, i64)>(&rt, &client, &order_counts()).await;
  assert_eq!(
    counts,
    [
      ("ada".to_owned(), 4),
      ("bob".to_owned(), 1),
      ("cy".to_owned(), 1)
    ]
  );
  let _ = rt.run(client.close()).await;
}

#[cfg(feature = "pg")]
#[tokio::test]
async fn live_postgres_statements_build_run_and_decode() {
  use id_effect::Effect;
  use id_effect_sql::pg::{PgPoolConfig, PgSqlClient, pg_pool_from_config};
  use id_effect_sql::{SqlClient, execute_query, fetch_optional_as, provide_sql_client};
  use id_effect_tokio::TokioRuntime;

  let test = "live_postgres_statements_build_run_and_decode";
  let required = std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1");
  let pool = match std::env::var("DATABASE_URL") {
    Ok(url) => pg_pool_from_config(PgPoolConfig::from_url(url).with_max_size(2)).await,
    Err(_) => {
      assert!(
        !required,
        "{test}: ID_EFFECT_REQUIRE_PG=1 but DATABASE_URL is not set"
      );
      eprintln!("SKIP {test}: DATABASE_URL is not set");
      return;
    }
  };
  let pool = match pool {
    Ok(pool) => pool,
    Err(e) => {
      assert!(!required, "{test}: cannot open a pool: {e:?}");
      eprintln!("SKIP {test}: cannot open a pool: {e:?}");
      return;
    }
  };
  let rt = TokioRuntime::current().expect("tokio");
  let client = PgSqlClient::new(pool);
  if let Exit::Failure(c) = rt.run(client.connect()).await {
    assert!(!required, "{test}: cannot connect: {c:?}");
    eprintln!("SKIP {test}: cannot connect: {c:?}");
    return;
  }
  async fn ok<A: Send + 'static>(
    rt: &TokioRuntime,
    client: &PgSqlClient,
    eff: Effect<A, SqlError, SqlClientEnv>,
  ) -> A {
    match rt
      .run(eff.provide_layer(provide_sql_client::<SqlError>(client.clone())))
      .await
    {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("statement failed: {c:?}"),
    }
  }

  for ddl in [
    "DROP TABLE IF EXISTS query_items",
    "CREATE TABLE query_items (id bigserial PRIMARY KEY, key uuid NOT NULL, title text NOT NULL, \
     due date NOT NULL, at timestamptz NOT NULL, price numeric(10, 2) NOT NULL, \
     meta jsonb NOT NULL, note text)",
  ] {
    assert!(matches!(
      rt.run(client.execute(ddl, &[])).await,
      Exit::Success(_)
    ));
  }

  let pg = Dialect::Postgres;
  let first = ok(
    &rt,
    &client,
    fetch_one_as::<Item, _, _>(pg, &insert(&item(0))),
  )
  .await;
  assert_eq!(first, item(first.id));
  let second = ok(
    &rt,
    &client,
    fetch_one_as::<Item, _, _>(pg, &insert(&item(0))),
  )
  .await;
  assert_eq!(
    ok(
      &rt,
      &client,
      fetch_all_as::<Item, _, _>(pg, &by_title("docs"))
    )
    .await,
    [item(first.id), item(second.id)]
  );

  let update = Query::update()
    .table("query_items")
    .value("note", "checked")
    .value("price", Decimal::new(500, 2))
    .and_where(Expr::col("id").eq(second.id))
    .to_owned();
  assert_eq!(ok(&rt, &client, execute_query(pg, &update)).await, 1);
  let note = Query::select()
    .columns(["note", "price"])
    .from("query_items")
    .and_where(Expr::col("id").eq(second.id))
    .to_owned();
  assert_eq!(
    ok(
      &rt,
      &client,
      fetch_optional_as::<(Option<String>, Decimal), _, _>(pg, &note)
    )
    .await,
    Some((Some("checked".into()), Decimal::new(500, 2)))
  );

  let delete = Query::delete()
    .from_table("query_items")
    .and_where(Expr::col("key").eq(Uuid::from_u128(0xfeed)))
    .and_where(Expr::col("at").lt(Timestamp::from_second(2_000_000_000).unwrap()))
    .to_owned();
  assert_eq!(ok(&rt, &client, execute_query(pg, &delete)).await, 2);
  assert_eq!(
    ok(
      &rt,
      &client,
      fetch_optional_as::<Item, _, _>(pg, &by_title("docs"))
    )
    .await,
    None
  );

  let _ = rt
    .run(client.execute("DROP TABLE IF EXISTS query_items", &[]))
    .await;
  let _ = rt.run(client.close()).await;
}
