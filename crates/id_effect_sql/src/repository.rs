//! Entities and repositories (feature `repository`): `#[derive(Entity)]` maps a struct to a
//! table, and [`Repository<T>`] runs its CRUD statements — built with [`sea_query`] and rendered
//! for a [`Dialect`] — over the [`SqlClient`] in the environment, or over any [`SqlExecutor`]
//! (an open transaction) with [`Repository::on`].
//!
//! - **Mapping** (data.orm #1). The table is `#[entity(table = "..")]` (default: the type name in
//!   snake case), the columns are the [`FromRow`] columns (`#[from_row(rename = "..")]`, else
//!   `#[schema("..")]`, else the field name), and `#[entity(id)]` marks the primary key
//!   (`#[entity(id, generated)]`: the database assigns it, so inserts omit it and read it back).
//!   `#[derive(Entity)]` needs `#[derive(FromRow)]` next to it; each column is written with
//!   [`ToSqlValue`].
//! - **Optimistic locking** (data.orm #5). An `#[entity(version)]` integer column is checked and
//!   incremented by every [`update`](Repository::update) (`UPDATE … SET version = version + 1
//!   WHERE id = ? AND version = ?`); when another writer got there first the update fails with
//!   [`SqlError::StaleVersion`] naming the entity and id (and [`SqlError::RowNotFound`] when the
//!   row is gone or filtered out).
//! - **Global filters** (data.orm #8) are AND-ed into every `SELECT`, `UPDATE` and `DELETE` the
//!   repository generates: `#[entity(soft_delete = "deleted_at")]` (rows whose column is not NULL
//!   are hidden, and [`delete`](Repository::delete) sets it to the current time),
//!   `#[entity(tenant = "org_id")]` (only the repository's tenant — [`Repository::with_tenant`],
//!   or [`CurrentTenant`] from the environment with [`Repository::for_current_tenant`] — and
//!   inserts write it), and `#[entity(scope = path::to::fn)]` (a `fn() -> impl
//!   Into<Condition>`, repeatable). A tenant-scoped entity without a tenant fails closed
//!   ([`SqlError::Unsupported`]). [`Repository::with_deleted`] lifts the soft-delete filter only;
//!   [`Repository::unscoped`] lifts every filter (the explicit escape hatch).
//!
//! Dialects: PostgreSQL and SQLite insert with `RETURNING`; MySQL inserts and then reads the row
//! back by id, so a `generated` id is [`SqlError::Unsupported`] there.
//!
//! ```rust
//! use id_effect::Exit;
//! use id_effect_sql::{
//!   Dialect, Entity, FromRow, Repository, SqlClientEnv, SqlError, SqlRow, SqlValue, TestSqlClient,
//!   sql_client_layer,
//! };
//!
//! #[derive(Clone, Debug, PartialEq, FromRow, Entity)]
//! #[entity(table = "accounts", soft_delete = "deleted_at", tenant = "org_id")]
//! struct Account {
//!   #[entity(id)]
//!   id: i64,
//!   name: String,
//!   #[entity(version)]
//!   version: i64,
//! }
//!
//! let repo = Repository::<Account>::new(Dialect::Postgres).with_tenant(7i64);
//! let test = TestSqlClient::new();
//! test.script_query(
//!   r#"SELECT "id", "name", "version" FROM "accounts" WHERE "org_id" = $1 AND "deleted_at" IS NULL AND "id" = $2 LIMIT $3"#,
//!   vec![SqlRow::from_pairs([
//!     ("id", SqlValue::I64(1)),
//!     ("name", "ada".into()),
//!     ("version", SqlValue::I64(3)),
//!   ])],
//! );
//! let found = repo
//!   .find::<SqlClientEnv, _>(1)
//!   .provide_layer(sql_client_layer::<SqlError>(test));
//! assert!(matches!(found.block_on(), Exit::Success(Some(a)) if a.name == "ada" && a.version == 3));
//! assert_eq!(Account::TABLE, "accounts");
//! ```

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use id_effect::{Effect, HList, Selector, fail, need, succeed};
use sea_query::{Condition, Expr, ExprTrait, IntoCondition, Order, Query, SelectStatement, Value};

use crate::client::{SqlClient, SqlClientService};
use crate::error::SqlError;
use crate::from_row::{FromRow, from_rows};
use crate::query_builder::{BuildSql, Dialect};
use crate::row::SqlRow;
use crate::transaction::{SqlTransaction, SqlTransactionService};
use crate::value::{SqlValue, ToSqlValue};

/// A struct mapped to a table: what `#[derive(Entity)]` generates (see the [module docs](self)).
///
/// The constants are the mapping; [`values`](Self::values) are the column values to write.
pub trait Entity: FromRow + Send + 'static {
  /// The primary key's type (`Debug` names it in [`SqlError::StaleVersion`]).
  type Id: ToSqlValue + fmt::Debug + Send + 'static;

  /// The Rust type name, for errors.
  const NAME: &'static str;
  /// The table.
  const TABLE: &'static str;
  /// Every mapped column, in field order (the `SELECT` list).
  const COLUMNS: &'static [&'static str];
  /// The primary key column.
  const ID: &'static str;
  /// Whether the database assigns the primary key (inserts omit it and read it back).
  const ID_GENERATED: bool = false;
  /// The optimistic-locking version column (an integer), if any.
  const VERSION: Option<&'static str> = None;
  /// The soft-delete column (NULL while the row is live, the deletion time after), if any.
  /// It need not be a field.
  const SOFT_DELETE: Option<&'static str> = None;
  /// The tenant (or owner) column every statement is scoped to, if any. It need not be a field.
  const TENANT: Option<&'static str> = None;

  /// The primary key.
  fn id(&self) -> Self::Id;

  /// Every mapped column with its value, in [`COLUMNS`](Self::COLUMNS) order.
  fn values(&self) -> Vec<(&'static str, SqlValue)>;

  /// Add one to the version field (what a successful [`Repository::update`] wrote). The default
  /// does nothing (no version column).
  fn bump_version(&mut self) {}

  /// The custom filters (`#[entity(scope = ..)]`), AND-ed into every statement. The default is
  /// no filter.
  fn scope() -> Condition {
    Condition::all()
  }
}

/// Capability: the tenant (or owner) value tenant-scoped entities are filtered by
/// ([`Repository::for_current_tenant`]), e.g. provided per request from the authenticated
/// principal.
#[derive(Clone, Debug, PartialEq)]
pub struct CurrentTenant(pub SqlValue);

impl CurrentTenant {
  /// The tenant `value` (`CurrentTenant::new(org_id)`).
  #[inline]
  pub fn new(value: impl ToSqlValue) -> Self {
    Self(value.to_sql_value())
  }
}

/// Something statements run on: the client ([`SqlClientService`], `Arc<dyn SqlClient>`) or an
/// open transaction ([`SqlTransactionService`], `Arc<dyn SqlTransaction>`).
pub trait SqlExecutor: Send + Sync + 'static {
  /// Run a read query and collect its rows.
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError>;
  /// Run a write statement; the affected row count.
  fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError>;
}

macro_rules! executor {
  ($ty:ty) => {
    impl SqlExecutor for $ty {
      fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError> {
        (**self).query(sql, params)
      }

      fn execute(&self, sql: &str, params: &[SqlValue]) -> Effect<u64, SqlError> {
        (**self).execute(sql, params)
      }
    }
  };
}

executor!(SqlClientService);
executor!(SqlTransactionService);
executor!(Arc<dyn SqlClient>);
executor!(Arc<dyn SqlTransaction>);

/// One page of rows ([`Repository::find_page`]): at most `limit` rows after skipping `offset`,
/// in primary-key order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Page {
  /// The most rows returned.
  pub limit: u64,
  /// The rows skipped first.
  pub offset: u64,
}

impl Page {
  /// `limit` rows after skipping `offset`.
  #[inline]
  pub fn new(limit: u64, offset: u64) -> Self {
    Self { limit, offset }
  }

  /// Page `number` (0-based) of `size` rows.
  #[inline]
  pub fn number(number: u64, size: u64) -> Self {
    Self::new(size, number.saturating_mul(size))
  }
}

/// Which global filters apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filters {
  /// Soft delete, tenant and custom scopes.
  All,
  /// Tenant and custom scopes ([`Repository::with_deleted`]).
  WithDeleted,
  /// None ([`Repository::unscoped`]).
  None,
}

/// A statement rendered for the repository's dialect.
type Stmt = (String, Vec<SqlValue>);

/// An operation ready to run on an executor.
type Run<A> = Box<dyn FnOnce(Arc<dyn SqlExecutor>) -> Effect<A, SqlError> + Send>;

fn ready<A: Send + 'static>(r: Result<A, SqlError>) -> Effect<A, SqlError> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

/// The rows of `stmt`, then `f`.
fn rows<A: Send + 'static>(
  (sql, params): Stmt,
  f: impl FnOnce(Vec<SqlRow>) -> Result<A, SqlError> + Send + 'static,
) -> Run<A> {
  Box::new(move |ex| ex.query(&sql, &params).flat_map(|rows| ready(f(rows))))
}

/// Whether `stmt` affected a row.
fn affected_any((sql, params): Stmt) -> Run<bool> {
  Box::new(move |ex| ex.execute(&sql, &params).map(|n| n > 0))
}

fn first<T: FromRow>(rows: Vec<SqlRow>) -> Result<Option<T>, SqlError> {
  rows.first().map(T::from_row).transpose()
}

fn the_row<T: FromRow>(rows: Vec<SqlRow>) -> Result<T, SqlError> {
  first(rows)?.ok_or(SqlError::RowNotFound)
}

/// `base AND cond`, leaving out an empty `cond`.
fn and(base: Condition, cond: Condition) -> Condition {
  if cond.is_empty() {
    base
  } else {
    base.add(cond)
  }
}

/// CRUD for the entity `T` (see the [module docs](self)): statements in its dialect, with the
/// global filters it is configured with.
///
/// Methods run on the [`SqlClientService`] in the environment `R`; [`on`](Self::on) binds the
/// same operations to a transaction. Every method renders its statement when called, so a
/// rendering or scope error fails the returned effect.
pub struct Repository<T> {
  dialect: Dialect,
  tenant: Option<SqlValue>,
  filters: Filters,
  entity: PhantomData<fn() -> T>,
}

impl<T> Clone for Repository<T> {
  fn clone(&self) -> Self {
    Self {
      dialect: self.dialect,
      tenant: self.tenant.clone(),
      filters: self.filters,
      entity: PhantomData,
    }
  }
}

impl<T> fmt::Debug for Repository<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Repository")
      .field("entity", &std::any::type_name::<T>())
      .field("dialect", &self.dialect)
      .field("tenant", &self.tenant)
      .field("filters", &self.filters)
      .finish()
  }
}

/// A [`Repository`] bound to one executor ([`Repository::on`]): the same operations, as
/// environment-free effects (inside a transaction: `.narrow()` them into its environment).
pub struct BoundRepository<T> {
  repo: Repository<T>,
  ex: Arc<dyn SqlExecutor>,
}

impl<T> Clone for BoundRepository<T> {
  fn clone(&self) -> Self {
    Self {
      repo: self.repo.clone(),
      ex: Arc::clone(&self.ex),
    }
  }
}

impl<T> fmt::Debug for BoundRepository<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("BoundRepository").field(&self.repo).finish()
  }
}

impl<T: Entity> Repository<T> {
  /// A repository rendering statements for `dialect`, with every filter on and no tenant.
  pub fn new(dialect: Dialect) -> Self {
    Self {
      dialect,
      tenant: None,
      filters: Filters::All,
      entity: PhantomData,
    }
  }

  /// A repository for `dialect` scoped to the [`CurrentTenant`] in the environment `R`.
  pub fn for_current_tenant<R, J>(dialect: Dialect) -> Effect<Self, SqlError, R>
  where
    R: Selector<CurrentTenant, J> + HList,
  {
    need::<CurrentTenant>().flat_map(move |t| succeed(Self::new(dialect).with_tenant(t.0.clone())))
  }

  /// This repository scoped to `tenant` (tenant-scoped statements filter by it, inserts write
  /// it).
  #[must_use]
  pub fn with_tenant(&self, tenant: impl ToSqlValue) -> Self {
    Self {
      tenant: Some(tenant.to_sql_value()),
      ..self.clone()
    }
  }

  /// This repository including soft-deleted rows (the tenant and custom filters still apply).
  #[must_use]
  pub fn with_deleted(&self) -> Self {
    Self {
      filters: Filters::WithDeleted,
      ..self.clone()
    }
  }

  /// This repository with no global filter: soft-deleted rows, every tenant and rows outside the
  /// custom scopes included. The explicit escape hatch (admin tools, data migrations). A tenant
  /// set with [`with_tenant`](Self::with_tenant) is still written on insert.
  #[must_use]
  pub fn unscoped(&self) -> Self {
    Self {
      filters: Filters::None,
      ..self.clone()
    }
  }

  /// The dialect statements are rendered in.
  #[inline]
  pub fn dialect(&self) -> Dialect {
    self.dialect
  }

  /// The tenant statements are scoped to, if any.
  #[inline]
  pub fn tenant(&self) -> Option<&SqlValue> {
    self.tenant.as_ref()
  }

  /// The same operations on `executor`, e.g. a transaction: `repo.on(tx).update(account)`.
  pub fn on(&self, executor: impl SqlExecutor) -> BoundRepository<T> {
    BoundRepository {
      repo: self.clone(),
      ex: Arc::new(executor),
    }
  }

  /// The tenant to scope by and write: `None` without a tenant column, or when unscoped without
  /// a tenant; an error when a scoped repository of a tenant-scoped entity has none.
  fn tenant_value(&self) -> Result<Option<SqlValue>, SqlError> {
    match (T::TENANT, &self.tenant) {
      (None, _) => Ok(None),
      (Some(_), Some(v)) => Ok(Some(v.clone())),
      (Some(_), None) if self.filters == Filters::None => Ok(None),
      (Some(col), None) => Err(SqlError::Unsupported(format!(
        "entity `{}` is scoped by tenant column `{col}` but the repository has no tenant \
         (Repository::with_tenant / for_current_tenant, or unscoped())",
        T::NAME
      ))),
    }
  }

  /// The global filters, without the soft-delete one when `soft_delete` is false.
  fn scope(&self, soft_delete: bool) -> Result<Condition, SqlError> {
    let tenant = self.tenant_value()?;
    let mut cond = Condition::all();
    if self.filters == Filters::None {
      return Ok(cond);
    }
    if let (Some(col), Some(v)) = (T::TENANT, tenant) {
      cond = cond.add(Expr::col(col).eq(Value::from(v)));
    }
    if let (Some(col), true, Filters::All) = (T::SOFT_DELETE, soft_delete, self.filters) {
      cond = cond.add(Expr::col(col).is_null());
    }
    Ok(and(cond, T::scope()))
  }

  fn id_is(id: &T::Id) -> Condition {
    Condition::all().add(Expr::col(T::ID).eq(Value::from(id.to_sql_value())))
  }

  fn render(&self, stmt: &impl BuildSql) -> Result<Stmt, SqlError> {
    stmt.build_sql(self.dialect)
  }

  /// `SELECT <columns> FROM <table> WHERE <filters> AND <cond>`.
  fn select(&self, cond: Condition) -> Result<SelectStatement, SqlError> {
    let mut stmt = Query::select();
    stmt.columns(T::COLUMNS.iter().copied()).from(T::TABLE);
    self.filter(&mut stmt, cond)?;
    Ok(stmt)
  }

  /// `WHERE <filters> AND <cond>`, left out when both are empty (sea-query would render
  /// `WHERE TRUE`).
  fn filter(&self, stmt: &mut SelectStatement, cond: Condition) -> Result<(), SqlError> {
    let cond = and(self.scope(true)?, cond);
    if !cond.is_empty() {
      stmt.cond_where(cond);
    }
    Ok(())
  }

  fn find_op(&self, id: &T::Id) -> Result<Run<Option<T>>, SqlError> {
    let stmt = self.select(Self::id_is(id))?.limit(1).to_owned();
    Ok(rows(self.render(&stmt)?, first))
  }

  fn find_by_op(&self, cond: Condition, page: Option<Page>) -> Result<Run<Vec<T>>, SqlError> {
    let mut stmt = self.select(cond)?;
    stmt.order_by(T::ID, Order::Asc);
    if let Some(page) = page {
      stmt.limit(page.limit).offset(page.offset);
    }
    Ok(rows(self.render(&stmt)?, |rows| from_rows(&rows)))
  }

  fn count_op(&self, cond: Condition) -> Result<Run<u64>, SqlError> {
    let mut stmt = Query::select();
    stmt.expr(Expr::cust("COUNT(*)")).from(T::TABLE);
    self.filter(&mut stmt, cond)?;
    Ok(rows(self.render(&stmt)?, |rows| {
      let n = rows
        .first()
        .ok_or(SqlError::RowNotFound)?
        .get::<i64>(0usize)?;
      Ok(u64::try_from(n).unwrap_or_default())
    }))
  }

  fn exists_stmt(&self, id: &T::Id) -> Result<Stmt, SqlError> {
    let stmt = Query::select()
      .column(T::ID)
      .from(T::TABLE)
      .cond_where(and(self.scope(true)?, Self::id_is(id)))
      .limit(1)
      .to_owned();
    self.render(&stmt)
  }

  fn exists_op(&self, id: &T::Id) -> Result<Run<bool>, SqlError> {
    Ok(rows(self.exists_stmt(id)?, |rows| Ok(!rows.is_empty())))
  }

  fn insert_op(&self, entity: T) -> Result<Run<T>, SqlError> {
    let tenant = self.tenant_value()?;
    let (mut columns, mut values) = (Vec::new(), Vec::new());
    for (col, v) in entity.values() {
      let generated = col == T::ID && T::ID_GENERATED;
      if generated || (tenant.is_some() && Some(col) == T::TENANT) {
        continue;
      }
      columns.push(col);
      values.push(Expr::val(Value::from(v)));
    }
    if let (Some(col), Some(v)) = (T::TENANT, tenant) {
      columns.push(col);
      values.push(Expr::val(Value::from(v)));
    }
    let mut stmt = Query::insert();
    stmt
      .into_table(T::TABLE)
      .columns(columns)
      .values_panic(values);
    if self.dialect != Dialect::MySql {
      stmt.returning(Query::returning().columns(T::COLUMNS.iter().copied()));
      return Ok(rows(self.render(&stmt)?, the_row));
    }
    if T::ID_GENERATED {
      return Err(SqlError::Unsupported(format!(
        "entity `{}`: MySQL has no RETURNING, so a generated id cannot be read back on insert",
        T::NAME
      )));
    }
    let (sql, params) = self.render(&stmt)?;
    let read_back = Query::select()
      .columns(T::COLUMNS.iter().copied())
      .from(T::TABLE)
      .cond_where(Self::id_is(&entity.id()))
      .to_owned();
    let read_back = rows(self.render(&read_back)?, the_row);
    Ok(Box::new(move |ex| {
      let again = Arc::clone(&ex);
      ex.execute(&sql, &params)
        .flat_map(move |_| read_back(again))
    }))
  }

  fn update_op(&self, mut entity: T) -> Result<Run<T>, SqlError> {
    let id = entity.id();
    let mut cond = and(self.scope(true)?, Self::id_is(&id));
    let mut stmt = Query::update();
    stmt.table(T::TABLE);
    for (col, v) in entity.values() {
      if Some(col) == T::VERSION {
        cond = cond.add(Expr::col(col).eq(Value::from(v)));
        stmt.value(col, Expr::col(col).add(1));
      } else if col != T::ID && Some(col) != T::TENANT && Some(col) != T::SOFT_DELETE {
        stmt.value(col, Value::from(v));
      }
    }
    stmt.cond_where(cond);
    let (sql, params) = self.render(&stmt)?;
    // Zero rows: with a version column, tell a stale version from a missing row.
    let stale = match T::VERSION {
      Some(_) => Some(self.exists_stmt(&id)?),
      None => None,
    };
    let id = format!("{id:?}");
    Ok(Box::new(move |ex| {
      ex.execute(&sql, &params).flat_map(move |n| {
        if n > 0 {
          entity.bump_version();
          return succeed(entity);
        }
        let Some((sql, params)) = stale else {
          return fail(SqlError::RowNotFound);
        };
        ex.query(&sql, &params).flat_map(move |rows| {
          fail(if rows.is_empty() {
            SqlError::RowNotFound
          } else {
            SqlError::StaleVersion {
              entity: T::NAME.to_owned(),
              id,
            }
          })
        })
      })
    }))
  }

  fn delete_op(&self, id: &T::Id) -> Result<Run<bool>, SqlError> {
    let Some(col) = T::SOFT_DELETE else {
      return self.hard_delete_op(id);
    };
    let mut stmt = Query::update();
    stmt.table(T::TABLE).value(
      col,
      Value::from(SqlValue::Timestamp(jiff::Timestamp::now())),
    );
    if let Some(version) = T::VERSION {
      stmt.value(version, Expr::col(version).add(1));
    }
    stmt.cond_where(and(self.scope(true)?, Self::id_is(id)));
    Ok(affected_any(self.render(&stmt)?))
  }

  fn hard_delete_op(&self, id: &T::Id) -> Result<Run<bool>, SqlError> {
    let stmt = Query::delete()
      .from_table(T::TABLE)
      .cond_where(and(self.scope(false)?, Self::id_is(id)))
      .to_owned();
    Ok(affected_any(self.render(&stmt)?))
  }

  /// Run `op` on the [`SqlClientService`] in `R`.
  fn in_env<A, R, I>(op: Result<Run<A>, SqlError>) -> Effect<A, SqlError, R>
  where
    A: Send + 'static,
    R: Selector<SqlClientService, I> + HList,
  {
    match op {
      Ok(run) => need::<SqlClientService>().flat_map(move |svc| run(svc).narrow()),
      Err(e) => fail(e),
    }
  }
}

impl<T: Entity> BoundRepository<T> {
  /// The repository this is bound from.
  #[inline]
  pub fn repository(&self) -> &Repository<T> {
    &self.repo
  }

  fn run<A: Send + 'static>(&self, op: Result<Run<A>, SqlError>) -> Effect<A, SqlError> {
    match op {
      Ok(run) => run(Arc::clone(&self.ex)),
      Err(e) => fail(e),
    }
  }
}

/// The operations, once: [`Repository`] runs them on the client in the environment,
/// [`BoundRepository`] on its executor.
macro_rules! operations {
  ($(
    $(#[$doc:meta])*
    fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty = $op:ident($($call:expr),*);
  )*) => {
    impl<T: Entity> Repository<T> {
      $(
        $(#[$doc])*
        pub fn $name<R, I>(&self, $($arg: $ty),*) -> Effect<$ret, SqlError, R>
        where
          R: Selector<SqlClientService, I> + HList,
        {
          Self::in_env(self.$op($($call),*))
        }
      )*
    }

    impl<T: Entity> BoundRepository<T> {
      $(
        $(#[$doc])*
        pub fn $name(&self, $($arg: $ty),*) -> Effect<$ret, SqlError> {
          self.run(self.repo.$op($($call),*))
        }
      )*
    }
  };
}

operations! {
  /// The row with primary key `id`, or `None` (also when a filter hides it).
  fn find(id: T::Id) -> Option<T> = find_op(&id);
  /// Every row the filters let through, in primary-key order.
  fn find_all() -> Vec<T> = find_by_op(Condition::all(), None);
  /// The rows matching `cond` (a sea-query condition or expression, e.g.
  /// `Expr::col("name").eq("ada")`), in primary-key order.
  fn find_by(cond: impl IntoCondition) -> Vec<T> = find_by_op(cond.into_condition(), None);
  /// One [`Page`] of the rows matching `cond`, in primary-key order.
  fn find_page(cond: impl IntoCondition, page: Page) -> Vec<T> =
    find_by_op(cond.into_condition(), Some(page));
  /// The number of rows the filters let through.
  fn count() -> u64 = count_op(Condition::all());
  /// The number of rows matching `cond`.
  fn count_by(cond: impl IntoCondition) -> u64 = count_op(cond.into_condition());
  /// Whether the row with primary key `id` exists (and the filters let it through).
  fn exists(id: T::Id) -> bool = exists_op(&id);
  /// Insert `entity` (without a `generated` id; with the repository's tenant) and return the
  /// stored row, generated id and column defaults included.
  fn insert(entity: T) -> T = insert_op(entity);
  /// Write every column of `entity` but the id, tenant and soft-delete columns, by id and
  /// within the filters. With a version column the update also requires the version `entity`
  /// was read with and increments it: the returned entity carries the new version, and a
  /// concurrent change fails with [`SqlError::StaleVersion`]. No such row:
  /// [`SqlError::RowNotFound`].
  fn update(entity: T) -> T = update_op(entity);
  /// Delete the row with primary key `id` — a soft delete (the soft-delete column set to now,
  /// the version incremented) when the entity has one. `false` when no row was deleted.
  fn delete(id: T::Id) -> bool = delete_op(&id);
  /// `DELETE` the row with primary key `id` even when the entity soft-deletes, soft-deleted or
  /// not (the tenant and custom filters still apply). `false` when no row was deleted.
  fn hard_delete(id: T::Id) -> bool = hard_delete_op(&id);
}
