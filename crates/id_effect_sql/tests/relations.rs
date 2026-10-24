//! Relations and eager loading (feature `repository`, data.orm #3): `#[entity(belongs_to(..))]`,
//! `has_many(..)`, `many_to_many(.., through = "..")`, `Repository::with` (one `IN` query per
//! relation per level, nested at most three levels) and the field metadata of the derive.
//!
//! The suite runs on SQLite in memory (feature `sqlite`); `tests/repository_relations_pg.rs`
//! runs the same suite on PostgreSQL in a container. ADR 0013 R1:
//! `m19_eager_loaded_children_scoped_to_tenant`, `relation_join_cannot_cross_tenant`.

#![cfg(feature = "repository")]

use std::sync::{Arc, Mutex};

use id_effect::{Cause, Effect, Exit, hlist};
use id_effect_sql::query_builder::sea_query::{Condition, Expr, ExprTrait};
use id_effect_sql::repository::{
  FieldMeta, Loaded, MAX_EAGER_DEPTH, Relation, RelationKind, RelationMeta,
};
use id_effect_sql::{
  Dialect, Entity, FromRow, Page, Repository, SqlClient, SqlClientEnv, SqlClientService, SqlError,
  SqlExecutor, SqlRow, SqlValue, TestSqlClient, ToSqlValue,
};

// ---- the model: orders of customers, with lines of products of vendors, and tags ----

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "rel_customers", tenant = "org_id", label = "shop.customer")]
pub struct Customer {
  #[entity(id)]
  pub id: i64,
  #[entity(editable, label = "shop.customer.name")]
  pub name: String,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "rel_orders", tenant = "org_id", soft_delete = "deleted_at")]
#[entity(belongs_to(Customer), has_many(Line, foreign_key = "order_id"))]
#[entity(many_to_many(Tag, through = "rel_order_tags", local_key = "order_id"))]
pub struct Order {
  #[entity(id)]
  pub id: i64,
  pub customer_id: Option<i64>,
  #[entity(editable)]
  pub title: String,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "rel_lines", tenant = "org_id", soft_delete = "deleted_at")]
#[entity(belongs_to(Product))]
pub struct Line {
  #[entity(id)]
  pub id: i64,
  pub order_id: i64,
  pub product_id: i64,
  pub qty: i64,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "rel_products", tenant = "org_id", belongs_to(Vendor))]
pub struct Product {
  #[entity(id)]
  pub id: i64,
  pub vendor_id: i64,
  pub name: String,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "rel_vendors", tenant = "org_id", has_many(Product))]
pub struct Vendor {
  #[entity(id)]
  pub id: i64,
  pub name: String,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "rel_tags", tenant = "org_id", soft_delete = "deleted_at")]
pub struct Tag {
  #[entity(id)]
  pub id: i64,
  pub name: String,
}

/// Every row of `rels` for one relation: ids, in order.
fn ids<P, C: Entity<Id = i64> + 'static>(parent: &Loaded<P>, rel: &Relation<P, C>) -> Vec<i64> {
  parent.get(rel).iter().map(|c| c.id()).collect()
}

// ---- metadata (no database) ----

#[test]
fn entity_fields_metadata_from_derive() {
  assert_eq!(Order::LABEL, "order");
  assert_eq!(
    Order::FIELDS,
    [
      FieldMeta::new("id", "id", "i64").label("order.id"),
      FieldMeta::new("customer_id", "customer_id", "i64")
        .nullable(true)
        .label("order.customer_id")
        .relation("customer"),
      FieldMeta::new("title", "title", "String")
        .editable(true)
        .label("order.title"),
    ]
  );
  assert_eq!(Customer::LABEL, "shop.customer");
  let name = Customer::FIELDS[1];
  assert_eq!(
    (name.name, name.label, name.editable, name.nullable),
    ("name", "shop.customer.name", true, false)
  );
  assert_eq!(
    Order::RELATIONS,
    [
      Order::customer().meta(),
      Order::lines().meta(),
      Order::tags().meta()
    ]
  );
  assert_eq!(
    Order::RELATIONS[0],
    RelationMeta::new(
      "customer",
      RelationKind::BelongsTo {
        foreign_key: "customer_id"
      },
      "Customer",
      "rel_customers"
    )
  );
  assert_eq!(
    Order::RELATIONS[2].kind,
    RelationKind::ManyToMany {
      through: "rel_order_tags",
      local_key: "order_id",
      foreign_key: "tag_id"
    }
  );
  assert_eq!(
    (Vendor::RELATIONS[0].name, Vendor::RELATIONS[0].kind),
    (
      "products",
      RelationKind::HasMany {
        foreign_key: "vendor_id"
      }
    )
  );
  assert_eq!(Line::FIELDS[2].relation, Some("product"));
  assert_eq!(Tag::FIELDS.len(), 2);
}

/// A hand-written entity (the 0.12 shape: no FIELDS, RELATIONS or LABEL).
#[derive(Clone, Debug, PartialEq)]
pub struct Legacy {
  pub id: i64,
}

impl FromRow for Legacy {
  fn from_row(row: &SqlRow) -> Result<Self, SqlError> {
    Ok(Self { id: row.get("id")? })
  }
}

impl Entity for Legacy {
  type Id = i64;
  const NAME: &'static str = "Legacy";
  const TABLE: &'static str = "legacy";
  const COLUMNS: &'static [&'static str] = &["id"];
  const ID: &'static str = "id";

  fn id(&self) -> i64 {
    self.id
  }

  fn values(&self) -> Vec<(&'static str, SqlValue)> {
    vec![("id", self.id.to_sql_value())]
  }
}

#[test]
fn manual_entity_impl_still_compiles() {
  assert_eq!(Legacy::LABEL, Legacy::NAME);
  assert!(Legacy::FIELDS.is_empty());
  assert!(Legacy::RELATIONS.is_empty());

  // A relation built by hand works the same way.
  let lines = Relation::<Legacy, Line>::has_many("lines", "order_id");
  let test = TestSqlClient::new();
  assert!(matches!(test.connect().block_on(), Exit::Success(())));
  test.script_query(
    r#"SELECT "id" FROM "legacy" ORDER BY "id" ASC"#,
    vec![SqlRow::from_pairs([("id", 7i64)])],
  );
  let line = |id: i64| {
    SqlRow::from_pairs([
      ("id", id),
      ("order_id", 7),
      ("product_id", 1),
      ("qty", 1),
      ("__id_effect_key", 7),
    ])
  };
  test.script_query(
    r#"SELECT "id", "order_id", "product_id", "qty", "order_id" AS "__id_effect_key" FROM "rel_lines" WHERE "org_id" = $1 AND "deleted_at" IS NULL AND "order_id" IN ($2) ORDER BY "id" ASC"#,
    vec![line(1), line(2)],
  );
  let found = Repository::<Legacy>::new(Dialect::Postgres)
    .with_tenant(1i64)
    .with(lines.clone())
    .find_all::<SqlClientEnv, _>()
    .provide_all(hlist![SqlClientService::new(test.clone())])
    .block_on();
  let Exit::Success(found) = found else {
    panic!("{found:?}")
  };
  assert_eq!(ids(&found[0], &lines), [1, 2]);
  assert_eq!(test.recorded_queries().len(), 2);
}

// ---- the live suite ----

/// One database under test.
pub struct Db {
  pub client: Arc<dyn SqlClient>,
  pub dialect: Dialect,
}

/// An executor counting (and recording) the queries it runs: the N+1 probe.
#[derive(Clone)]
pub struct Probe {
  inner: Arc<dyn SqlClient>,
  queries: Arc<Mutex<Vec<String>>>,
}

impl Probe {
  pub fn new(db: &Db) -> Self {
    Self {
      inner: Arc::clone(&db.client),
      queries: Arc::default(),
    }
  }

  pub fn queries(&self) -> Vec<String> {
    self.queries.lock().expect("probe").clone()
  }
}

impl SqlExecutor for Probe {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
    self.queries.lock().expect("probe").push(sql.to_owned());
    SqlClient::query(&*self.inner, sql, params)
  }

  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
    SqlClient::execute(&*self.inner, sql, params)
  }
}

impl Db {
  pub async fn run_plain<A: Send + 'static>(
    &self,
    eff: Effect<A, SqlError>,
  ) -> Result<A, SqlError> {
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

  pub async fn run<A: Send + 'static>(
    &self,
    eff: Effect<A, SqlError, SqlClientEnv>,
  ) -> Result<A, SqlError> {
    let eff = eff.provide_all(hlist![SqlClientService(Arc::clone(&self.client))]);
    self.run_plain(eff).await
  }

  pub async fn ok<A: Send + 'static>(&self, eff: Effect<A, SqlError>) -> A {
    self
      .run_plain(eff)
      .await
      .unwrap_or_else(|e| panic!("statement failed: {e}"))
  }

  async fn exec(&self, sql: &str) {
    self
      .run_plain(SqlClient::execute(&*self.client, sql, &[]))
      .await
      .unwrap_or_else(|e| panic!("{sql}: {e}"));
  }

  /// The tables and rows every suite body reads: org 1's data plus rows crafted to point across
  /// tenants (org 2) and soft-deleted rows.
  pub async fn seed(&self) {
    let ts = if self.dialect == Dialect::Postgres {
      "TIMESTAMPTZ"
    } else {
      "TIMESTAMP"
    };
    let tables = [
      ("rel_customers", "id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, name TEXT NOT NULL".to_owned()),
      ("rel_vendors", "id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, name TEXT NOT NULL".to_owned()),
      (
        "rel_products",
        "id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, vendor_id BIGINT NOT NULL, name TEXT NOT NULL"
          .to_owned(),
      ),
      (
        "rel_orders",
        format!(
          "id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, customer_id BIGINT, title TEXT NOT NULL, \
           deleted_at {ts}"
        ),
      ),
      (
        "rel_lines",
        format!(
          "id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, order_id BIGINT NOT NULL, \
           product_id BIGINT NOT NULL, qty BIGINT NOT NULL, deleted_at {ts}"
        ),
      ),
      (
        "rel_tags",
        format!("id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, name TEXT NOT NULL, deleted_at {ts}"),
      ),
      ("rel_order_tags", "order_id BIGINT NOT NULL, tag_id BIGINT NOT NULL".to_owned()),
    ];
    for (table, columns) in tables {
      self.exec(&format!("DROP TABLE IF EXISTS {table}")).await;
      self
        .exec(&format!("CREATE TABLE {table} ({columns})"))
        .await;
    }
    for sql in [
      "INSERT INTO rel_customers (id, org_id, name) VALUES (1, 1, 'ada'), (2, 1, 'bob'), \
       (3, 2, 'eve')",
      "INSERT INTO rel_vendors (id, org_id, name) VALUES (1, 1, 'acme'), (2, 2, 'evil')",
      "INSERT INTO rel_products (id, org_id, vendor_id, name) VALUES (1, 1, 1, 'bolt'), \
       (2, 1, 1, 'nut'), (3, 2, 2, 'spy'), (4, 1, 2, 'crafted')",
      // Order 5 is org 1's but names org 2's customer.
      "INSERT INTO rel_orders (id, org_id, customer_id, title) VALUES (1, 1, 1, 'first'), \
       (2, 1, 2, 'second'), (3, 1, NULL, 'third'), (4, 2, 3, 'theirs'), (5, 1, 3, 'crafted')",
      // Line 4 is org 2's but points at org 1's order 1; line 5 is soft-deleted; line 7 names
      // product 4, whose vendor is org 2's.
      "INSERT INTO rel_lines (id, org_id, order_id, product_id, qty) VALUES (1, 1, 1, 1, 2), \
       (2, 1, 1, 2, 3), (3, 1, 2, 1, 1), (4, 2, 1, 3, 9), (5, 1, 2, 2, 7), (6, 2, 4, 3, 1), \
       (7, 1, 3, 4, 1)",
      "UPDATE rel_lines SET deleted_at = CURRENT_TIMESTAMP WHERE id = 5",
      "INSERT INTO rel_tags (id, org_id, name) VALUES (1, 1, 'red'), (2, 1, 'blue'), \
       (3, 2, 'secret'), (4, 1, 'gone')",
      "UPDATE rel_tags SET deleted_at = CURRENT_TIMESTAMP WHERE id = 4",
      // Join rows to org 2's tag 3 and the deleted tag 4.
      "INSERT INTO rel_order_tags (order_id, tag_id) VALUES (1, 1), (1, 2), (2, 1), (1, 3), \
       (2, 4), (4, 3)",
    ] {
      self.exec(sql).await;
    }
  }
}

fn orders(db: &Db, org: i64) -> Repository<Order> {
  Repository::<Order>::new(db.dialect).with_tenant(org)
}

pub async fn eager_load_issues_one_query_per_relation(db: &Db) {
  db.seed().await;
  let probe = Probe::new(db);
  let eager = orders(db, 1)
    .on(probe.clone())
    .with(Order::lines())
    .with(Order::customer())
    .with(Order::tags());
  let loaded = db.ok(eager.find_all()).await;
  // One query for the orders, then one per relation, whatever the number of orders.
  assert_eq!(probe.queries().len(), 1 + 3, "{:#?}", probe.queries());
  assert_eq!(
    loaded.iter().map(|o| o.id).collect::<Vec<_>>(),
    [1, 2, 3, 5]
  );
  let (lines, customer, tags) = (Order::lines(), Order::customer(), Order::tags());
  assert_eq!(ids(&loaded[0], &lines), [1, 2]);
  assert_eq!(ids(&loaded[1], &lines), [3]);
  assert_eq!(ids(&loaded[2], &lines), [7]);
  assert_eq!(ids(&loaded[3], &lines), [] as [i64; 0]);
  assert_eq!(
    loaded[0].one(&customer).map(|c| c.name.as_str()),
    Some("ada")
  );
  assert_eq!(
    loaded[1].one(&customer).map(|c| c.name.as_str()),
    Some("bob")
  );
  assert!(loaded[2].one(&customer).is_none(), "a NULL foreign key");
  assert_eq!(ids(&loaded[1], &tags), [1]);
  assert_eq!(loaded[0].relations(), ["lines", "customer", "tags"]);
  for sql in &probe.queries()[1..] {
    assert!(sql.contains(" IN ("), "{sql}");
  }

  // The same through the environment, with a filter, a page and one row.
  let in_env = orders(db, 1).with(Order::lines());
  let page = db
    .run(in_env.find_page(Expr::col("id").gte(2), Page::new(1, 0)))
    .await
    .unwrap();
  assert_eq!(
    (page.len(), page[0].id, ids(&page[0], &lines)),
    (1, 2, vec![3])
  );
  let by = db
    .run(in_env.find_by(Expr::col("title").eq("first")))
    .await
    .unwrap();
  assert_eq!(ids(&by[0], &lines), [1, 2]);
  let one = db.run(in_env.find(1)).await.unwrap().expect("order 1");
  assert_eq!(ids(&one, &lines), [1, 2]);
  assert!(db.run(in_env.find(4)).await.unwrap().is_none(), "org 2's");

  // Entities already read: only the relation query runs.
  let probe = Probe::new(db);
  let plain = db.run(orders(db, 1).find_all()).await.unwrap();
  let loaded = db
    .ok(
      orders(db, 1)
        .with(Order::lines())
        .on(probe.clone())
        .load(plain),
    )
    .await;
  assert_eq!((loaded.len(), probe.queries().len()), (4, 1));

  // Nothing to load: no relation query at all.
  let probe = Probe::new(db);
  let none = db
    .ok(
      orders(db, 1)
        .on(probe.clone())
        .with(Order::lines())
        .find_by(Expr::col("id").eq(99)),
    )
    .await;
  assert!(none.is_empty());
  assert_eq!(probe.queries().len(), 1);
}

pub async fn many_to_many_through_join_table(db: &Db) {
  db.seed().await;
  let tags = Order::tags();
  let loaded = db
    .run(orders(db, 1).with(tags.clone()).find_all())
    .await
    .unwrap();
  let names = |o: &Loaded<Order>| {
    o.get(&tags)
      .iter()
      .map(|t| t.name.clone())
      .collect::<Vec<_>>()
  };
  assert_eq!(names(&loaded[0]), ["red", "blue"]);
  assert_eq!(names(&loaded[1]), ["red"], "a tag shared by two orders");
  assert!(names(&loaded[2]).is_empty());
  // A tag reached twice is the same row.
  assert_eq!(
    loaded[0].get(&tags)[0].entity(),
    loaded[1].get(&tags)[0].entity()
  );
  // The soft-delete filter lifted: the deleted tag joins in, org 2's still does not.
  let with_deleted = db
    .run(orders(db, 1).with_deleted().with(tags.clone()).find(2))
    .await
    .unwrap()
    .expect("order 2");
  assert_eq!(ids(&with_deleted, &tags), [1, 4]);
}

pub async fn nested_eager_load_three_levels(db: &Db) {
  db.seed().await;
  let lines = Order::lines().with(Line::product().with(Product::vendor()));
  assert_eq!(lines.depth(), MAX_EAGER_DEPTH);
  let probe = Probe::new(db);
  let loaded = db
    .ok(
      orders(db, 1)
        .on(probe.clone())
        .with(lines.clone())
        .find_all(),
    )
    .await;
  assert_eq!(probe.queries().len(), 1 + 3, "one query per level");
  let first = &loaded[0].get(&lines)[1];
  let product = first.one(&Line::product()).expect("product");
  assert_eq!(product.name, "nut");
  let vendor = product.one(&Product::vendor()).expect("vendor");
  assert_eq!(vendor.name, "acme");
  // Line 7's product is org 1's, but its vendor is org 2's: not loaded.
  let crafted = &loaded[2].get(&lines)[0];
  let product = crafted.one(&Line::product()).expect("product 4");
  assert!(product.one(&Product::vendor()).is_none());

  // A fourth level is refused before any query runs.
  let probe = Probe::new(db);
  let too_deep =
    Order::lines().with(Line::product().with(Product::vendor().with(Vendor::products())));
  let err = db
    .run_plain(orders(db, 1).on(probe.clone()).with(too_deep).find_all())
    .await
    .unwrap_err();
  assert!(
    matches!(&err, SqlError::Unsupported(m) if m.contains("at most 3")),
    "{err:?}"
  );
  assert!(probe.queries().is_empty());
}

pub async fn m19_eager_loaded_children_scoped_to_tenant(db: &Db) {
  db.seed().await;
  let (lines, customer) = (Order::lines(), Order::customer());
  let org1 = db
    .run(
      orders(db, 1)
        .with(lines.clone())
        .with(customer.clone())
        .find_all(),
    )
    .await
    .unwrap();
  // Org 2's line 4 points at order 1; the deleted line 5 at order 2: neither loads.
  assert_eq!(ids(&org1[0], &lines), [1, 2]);
  assert_eq!(ids(&org1[1], &lines), [3]);
  // Order 5 names org 2's customer: no customer.
  assert_eq!(org1[3].id, 5);
  assert!(org1[3].one(&customer).is_none());
  // Org 2 sees its own order with its own children only.
  let org2 = db
    .run(
      orders(db, 2)
        .with(lines.clone())
        .with(customer.clone())
        .find_all(),
    )
    .await
    .unwrap();
  assert_eq!(org2.len(), 1);
  assert_eq!(ids(&org2[0], &lines), [6]);
  assert_eq!(org2[0].one(&customer).map(|c| c.id), Some(3));
  // No tenant fails closed, and nothing is read.
  let probe = Probe::new(db);
  let err = db
    .run_plain(
      Repository::<Order>::new(db.dialect)
        .on(probe.clone())
        .with(lines.clone())
        .find_all(),
    )
    .await
    .unwrap_err();
  assert!(matches!(err, SqlError::Unsupported(_)), "{err:?}");
  assert!(probe.queries().is_empty());
  // The explicit escape hatch lifts the children's filters too.
  let all = db
    .run(
      Repository::<Order>::new(db.dialect)
        .unscoped()
        .with(lines.clone())
        .find(1),
    )
    .await
    .unwrap()
    .expect("order 1");
  assert_eq!(ids(&all, &lines), [1, 2, 4]);
}

pub async fn relation_join_cannot_cross_tenant(db: &Db) {
  db.seed().await;
  let tags = Order::tags();
  // Join rows pair org 1's order 1 with org 2's tag 3: the join re-applies the tag's tenant.
  let order = db
    .run(orders(db, 1).with(tags.clone()).find(1))
    .await
    .unwrap()
    .expect("order 1");
  assert_eq!(ids(&order, &tags), [1, 2]);
  assert!(
    order.get(&tags).iter().all(|t| t.name != "secret"),
    "org 2's tag"
  );
  // And its soft-delete filter: order 2's deleted tag 4 is gone.
  let order = db
    .run(orders(db, 1).with(tags.clone()).find(2))
    .await
    .unwrap()
    .expect("order 2");
  assert_eq!(ids(&order, &tags), [1]);
  // From org 2's side, only its own tag.
  let theirs = db
    .run(orders(db, 2).with(tags.clone()).find_by(Condition::all()))
    .await
    .unwrap();
  assert_eq!(ids(&theirs[0], &tags), [3]);
}

/// One `#[tokio::test]` per suite body for the backend `$make` builds (`async fn(&str) -> Option<Db>`;
/// `None` skips); `$suite` is the module the bodies live in.
#[allow(unused_macros)]
macro_rules! relations_suite {
  ($make:path, $($suite:ident)::+) => {
    relations_suite!(@one $make, [$($suite)::+], eager_load_issues_one_query_per_relation);
    relations_suite!(@one $make, [$($suite)::+], many_to_many_through_join_table);
    relations_suite!(@one $make, [$($suite)::+], nested_eager_load_three_levels);
    relations_suite!(@one $make, [$($suite)::+], m19_eager_loaded_children_scoped_to_tenant);
    relations_suite!(@one $make, [$($suite)::+], relation_join_cannot_cross_tenant);
  };
  (@one $make:path, [$($suite:ident)::+], $name:ident) => {
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn $name() {
      let Some(db) = $make(stringify!($name)).await else {
        return;
      };
      $($suite::)+$name(&db).await;
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

  relations_suite!(memory, super);
}
