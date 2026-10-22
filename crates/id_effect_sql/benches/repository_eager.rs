//! Eager loading against the N+1 pattern it replaces (0.13, `r13-e-benches`): the same 100
//! parents with 10 children each, read through [`Repository`] on SQLite in memory, in one run.
//!
//! - `orm/eager_100x10`: `Repository::with(Order::lines()).find_all()`, one query for the
//!   parents and one `IN (…)` query for all their children (2 queries), returning
//!   `Vec<Loaded<Order>>`.
//! - `orm/n_plus_one_100x10`: `find_all()` for the parents, then one `find_by(order_id = ?)` per
//!   parent, awaited one after the other (101 queries): the loop eager loading removes.
//! - `orm/flat_100x10` (report only): the same rows as two plain `find_all()` reads, with no
//!   relation work at all, the floor eager loading cannot go below on this driver.
//!
//! All three read through the default scoping (tenant column and soft delete, ADR 0013 R1),
//! decode every row into the entity and run as one effect on the bench's tokio runtime, so the
//! difference is the statements. `docs/benchmarks/gates.toml` gates the first two as a `pair`
//! inside one run: `orm/eager_100x10 / orm/n_plus_one_100x10 ≤ 0.25`.
//!
//! Run: `cargo bench -p id_effect_sql --features repository,sqlite --bench repository_eager`

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect::{Cause, Effect, Exit, succeed};
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait};
use id_effect_sql::repository::{BoundRepository, Loaded};
use id_effect_sql::sqlite::{SqlitePoolConfig, SqliteSqlClient, sqlite_pool_from_config};
use id_effect_sql::{Dialect, Entity, FromRow, Repository, SqlClient, SqlError};

/// Parents.
const PARENTS: i64 = 100;
/// Children per parent.
const CHILDREN: i64 = 10;
/// The tenant every row belongs to.
const ORG: i64 = 1;

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "bench_orders", tenant = "org_id", soft_delete = "deleted_at")]
#[entity(has_many(Line, foreign_key = "order_id", name = "lines"))]
struct Order {
  #[entity(id)]
  id: i64,
  title: String,
}

#[derive(Clone, Debug, PartialEq, FromRow, Entity)]
#[entity(table = "bench_lines", tenant = "org_id", soft_delete = "deleted_at")]
struct Line {
  #[entity(id)]
  id: i64,
  order_id: i64,
  sku: String,
  qty: i64,
}

/// Each order with its lines, as the N+1 loop collects them.
type OrderLines = Vec<(Order, Vec<Line>)>;

fn runtime() -> tokio::runtime::Runtime {
  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .expect("tokio runtime")
}

/// Run `eff` on the current tokio runtime's effect runtime.
async fn run<A: Send + 'static>(eff: Effect<A, SqlError>) -> A {
  match id_effect_tokio::TokioRuntime::current()
    .expect("tokio")
    .run(eff)
    .await
  {
    Exit::Success(a) => a,
    Exit::Failure(Cause::Fail(e)) => panic!("statement failed: {e}"),
    Exit::Failure(cause) => panic!("unexpected cause: {cause:?}"),
  }
}

/// A fresh in-memory database with `PARENTS` orders of `CHILDREN` lines each.
async fn seeded() -> Arc<dyn SqlClient> {
  let pool = sqlite_pool_from_config(SqlitePoolConfig::memory().with_max_size(1))
    .await
    .expect("sqlite pool");
  let client: Arc<dyn SqlClient> = Arc::new(SqliteSqlClient::new(pool));
  let orders = (1..=PARENTS)
    .map(|id| format!("({id}, {ORG}, 'order {id}')"))
    .collect::<Vec<_>>()
    .join(", ");
  let lines = (1..=PARENTS)
    .flat_map(|order| {
      (0..CHILDREN).map(move |n| {
        let id = (order - 1) * CHILDREN + n + 1;
        format!("({id}, {ORG}, {order}, 'sku-{id}', {n})")
      })
    })
    .collect::<Vec<_>>()
    .join(", ");
  for sql in [
    "CREATE TABLE bench_orders (id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, \
     title TEXT NOT NULL, deleted_at TIMESTAMP)"
      .to_owned(),
    "CREATE TABLE bench_lines (id BIGINT PRIMARY KEY, org_id BIGINT NOT NULL, \
     order_id BIGINT NOT NULL, sku TEXT NOT NULL, qty BIGINT NOT NULL, deleted_at TIMESTAMP)"
      .to_owned(),
    // The foreign key is indexed, as it would be in a real schema: the N+1 side is not a scan.
    "CREATE INDEX bench_lines_order ON bench_lines (order_id)".to_owned(),
    format!("INSERT INTO bench_orders (id, org_id, title) VALUES {orders}"),
    format!("INSERT INTO bench_lines (id, org_id, order_id, sku, qty) VALUES {lines}"),
  ] {
    run(SqlClient::execute(&*client, &sql, &[])).await;
  }
  client
}

fn orders(client: &Arc<dyn SqlClient>) -> BoundRepository<Order> {
  Repository::<Order>::new(Dialect::Sqlite)
    .with_tenant(ORG)
    .on(Arc::clone(client))
}

fn lines(client: &Arc<dyn SqlClient>) -> BoundRepository<Line> {
  Repository::<Line>::new(Dialect::Sqlite)
    .with_tenant(ORG)
    .on(Arc::clone(client))
}

/// Every order with its lines: the parents, then one relation query for all of them.
fn eager(client: &Arc<dyn SqlClient>) -> Effect<Vec<Loaded<Order>>, SqlError> {
  orders(client).with(Order::lines()).find_all()
}

/// Every order with its lines: the parents, then one query per parent, in sequence.
fn n_plus_one(client: &Arc<dyn SqlClient>) -> Effect<OrderLines, SqlError> {
  let lines = lines(client);
  orders(client).find_all().flat_map(move |orders| {
    let mut all: Effect<OrderLines, SqlError> = succeed(Vec::with_capacity(orders.len()));
    for order in orders {
      let lines = lines.clone();
      all = all.flat_map(move |mut acc| {
        lines
          .find_by(Expr::col("order_id").eq(order.id))
          .map(move |children| {
            acc.push((order, children));
            acc
          })
      });
    }
    all
  })
}

/// The same rows with no relation work: every order, then every line.
fn flat(client: &Arc<dyn SqlClient>) -> Effect<(Vec<Order>, Vec<Line>), SqlError> {
  let lines = lines(client);
  orders(client)
    .find_all()
    .flat_map(move |orders| lines.find_all().map(move |lines| (orders, lines)))
}

fn bench_eager(c: &mut Criterion) {
  let rt = runtime();
  let client = rt.block_on(seeded());

  // The shapes read the same rows before anything is timed.
  let rel = Order::lines();
  let by_eager: OrderLines = rt
    .block_on(run(eager(&client)))
    .into_iter()
    .map(|order| {
      let children = order.get(&rel).iter().map(|l| l.entity().clone()).collect();
      (order.into_entity(), children)
    })
    .collect();
  let by_loop = rt.block_on(run(n_plus_one(&client)));
  let (flat_orders, flat_lines) = rt.block_on(run(flat(&client)));
  assert_eq!(by_eager.len(), PARENTS as usize);
  assert!(
    by_eager
      .iter()
      .all(|(_, lines)| lines.len() == CHILDREN as usize)
  );
  assert_eq!(by_eager, by_loop);
  assert_eq!(
    (flat_orders.len(), flat_lines.len()),
    (PARENTS as usize, (PARENTS * CHILDREN) as usize)
  );

  let mut group = c.benchmark_group("orm");
  group.bench_function("eager_100x10", |b| {
    b.iter(|| black_box(rt.block_on(run(eager(&client)))));
  });
  group.bench_function("n_plus_one_100x10", |b| {
    b.iter(|| black_box(rt.block_on(run(n_plus_one(&client)))));
  });
  group.bench_function("flat_100x10", |b| {
    b.iter(|| black_box(rt.block_on(run(flat(&client)))));
  });
  group.finish();
}

criterion_group!(benches, bench_eager);
criterion_main!(benches);
