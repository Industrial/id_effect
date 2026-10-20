//! SQL text as a type (ADR 0013 R5, M16): [`Sql`] for the statements the effect-level
//! functions accept, and the [`RawSqlCapability`] that [`query_raw`] / [`execute_raw`] need for
//! SQL built at run time.
//!
//! [`query`](crate::query), [`execute`](crate::execute) and the functions built on them take
//! `impl Into<Sql>`, and only a `&'static str` converts: a literal (or a `const`) compiles, a
//! `String` from `format!` or concatenation does not, so user input cannot be spliced into a
//! statement by accident. Values go in as bound parameters. SQL that really is assembled at run
//! time is a [`RawSql`], run by [`query_raw`] / [`execute_raw`], which also need a
//! [`RawSqlCapability`] in the environment; its only constructor is
//! [`RawSqlCapability::layer_dangerous`], which records a posture opt-out. Statements this crate
//! renders itself (the query builder, and the repository through it) are trusted internally.
//!
//! The [`SqlClient`](crate::SqlClient) trait's `&str` methods stay the driver boundary.
//!
//! ```rust
//! use id_effect::{Exit, HCons};
//! use id_effect_sql::{
//!   RawSql, RawSqlCapability, RawSqlEnv, SqlClientEnv, SqlClientService, SqlError, SqlRow,
//!   TestSqlClient, params, query, query_raw, sql_client_layer,
//! };
//!
//! let test = TestSqlClient::new();
//! test.script_query("SELECT id FROM users WHERE active = $1", vec![SqlRow::default()]);
//! let rows = query::<SqlClientEnv, _>("SELECT id FROM users WHERE active = $1", params![true])
//!   .provide_layer(sql_client_layer::<SqlError>(test.clone()));
//! assert!(matches!(rows.block_on(), Exit::Success(r) if r.len() == 1));
//!
//! // A table chosen at run time: raw SQL, behind the capability.
//! let table = "audit_2026";
//! let raw = RawSql::new(format!("SELECT count(*) FROM {table}"));
//! let layer = sql_client_layer::<SqlError>(test).merge(RawSqlCapability::layer_dangerous());
//! let counted =
//!   query_raw::<HCons<SqlClientService, RawSqlEnv>, _, _>(raw, vec![]).provide_layer(layer);
//! assert!(matches!(counted.block_on(), Exit::Success(_)));
//! ```

use std::borrow::Cow;
use std::fmt;

use id_effect::observability::posture::record_dangerous;
use id_effect::{Effect, HCons, HList, HNil, Layer, Selector, hlist, need};

use crate::client::SqlClientService;
use crate::error::SqlError;
use crate::row::SqlRow;
use crate::value::SqlValue;

/// The SQL text of a statement: a `&'static str` (a literal or a `const`), never a run-time
/// `String` (use [`RawSql`] for that).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Sql(Cow<'static, str>);

impl Sql {
  /// The statement `sql` (usable in a `const`).
  #[inline]
  pub const fn new(sql: &'static str) -> Self {
    Self(Cow::Borrowed(sql))
  }

  /// SQL this crate rendered itself (the query builder, and the repository through it).
  #[cfg(feature = "query")]
  #[inline]
  pub(crate) fn trusted(sql: String) -> Self {
    Self(Cow::Owned(sql))
  }

  /// The SQL text.
  #[inline]
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl From<&'static str> for Sql {
  #[inline]
  fn from(sql: &'static str) -> Self {
    Self::new(sql)
  }
}

impl AsRef<str> for Sql {
  #[inline]
  fn as_ref(&self) -> &str {
    self.as_str()
  }
}

impl fmt::Display for Sql {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// SQL text assembled at run time, for [`query_raw`] / [`execute_raw`]. Building one is free;
/// running it needs a [`RawSqlCapability`]. Whatever goes into it is the caller's
/// responsibility: bind values as parameters, and splice in only identifiers checked against
/// an allowlist.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RawSql(String);

impl RawSql {
  /// The raw statement `sql`.
  #[inline]
  pub fn new(sql: impl Into<String>) -> Self {
    Self(sql.into())
  }

  /// The SQL text.
  #[inline]
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for RawSql {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// Capability: permission to run [`RawSql`] ([`query_raw`] / [`execute_raw`]). It has no public
/// constructor; [`RawSqlCapability::layer_dangerous`] provides it.
#[derive(Clone, Debug)]
pub struct RawSqlCapability {
  _private: (),
}

/// The typed environment holding only [`RawSqlCapability`].
pub type RawSqlEnv = HCons<RawSqlCapability, HNil>;

impl RawSqlCapability {
  /// The [`Layer`] providing the capability (merge it with the client's layer). Records the
  /// posture opt-out `id_effect_sql::RawSqlCapability::layer_dangerous` (ADR 0013 R10).
  pub fn layer_dangerous<E: Send + 'static>() -> Layer<RawSqlEnv, E> {
    record_dangerous(
      "id_effect_sql::RawSqlCapability::layer_dangerous",
      "M16: SQL built at run time can splice untrusted input into a statement",
    );
    Layer::succeed(hlist![RawSqlCapability { _private: () }])
  }
}

/// [`query`](crate::query) for SQL built at run time: needs the [`SqlClientService`] and a
/// [`RawSqlCapability`] in `R`.
pub fn query_raw<R, I, J>(sql: RawSql, params: Vec<SqlValue>) -> Effect<Vec<SqlRow>, SqlError, R>
where
  R: Selector<SqlClientService, I> + Selector<RawSqlCapability, J> + HList,
{
  need::<RawSqlCapability>().flat_map(move |_| {
    need::<SqlClientService>().flat_map(move |c| c.query(sql.as_str(), &params).narrow())
  })
}

/// [`execute`](crate::execute) for SQL built at run time: needs the [`SqlClientService`] and a
/// [`RawSqlCapability`] in `R`.
pub fn execute_raw<R, I, J>(sql: RawSql, params: Vec<SqlValue>) -> Effect<u64, SqlError, R>
where
  R: Selector<SqlClientService, I> + Selector<RawSqlCapability, J> + HList,
{
  need::<RawSqlCapability>().flat_map(move |_| {
    need::<SqlClientService>().flat_map(move |c| c.execute(sql.as_str(), &params).narrow())
  })
}

#[cfg(test)]
mod tests {
  use id_effect::observability::posture::dangerous_opt_outs;

  use super::*;
  use crate::client::tests::run;
  use crate::{SqlClientEnv, TestSqlClient, execute, query, sql_client_layer};

  /// The client and the raw-SQL capability.
  type RawEnv = HCons<SqlClientService, RawSqlEnv>;

  const BY_ID: Sql = Sql::new("SELECT id FROM users WHERE id = $1");

  #[test]
  fn m16_literal_query_still_compiles() {
    let test = TestSqlClient::new();
    test.script_query(BY_ID.as_str(), vec![SqlRow::default()]);
    test.script_execute("DELETE FROM t", 2);
    let layer = || sql_client_layer::<SqlError>(test.clone());
    let rows =
      run(query::<SqlClientEnv, _>(BY_ID, crate::params![1i64]).provide_layer(layer())).unwrap();
    assert_eq!(rows.len(), 1);
    let n =
      run(execute::<SqlClientEnv, _>("DELETE FROM t", vec![]).provide_layer(layer())).unwrap();
    assert_eq!(n, 2);
    assert_eq!(Sql::from("SELECT 1").to_string(), "SELECT 1");
    assert_eq!(Sql::new("SELECT 2").as_ref(), "SELECT 2");
  }

  #[test]
  fn raw_sql_runs_with_the_capability_and_records_the_opt_out() {
    let test = TestSqlClient::new();
    let table = "audit_2026";
    let sql = RawSql::new(format!("DELETE FROM {table}"));
    test.script_execute(sql.as_str(), 4);
    test.script_query(sql.as_str(), vec![SqlRow::default()]);
    let layer =
      || sql_client_layer::<SqlError>(test.clone()).merge(RawSqlCapability::layer_dangerous());
    assert_eq!(
      run(execute_raw::<RawEnv, _, _>(sql.clone(), vec![]).provide_layer(layer())),
      Ok(4)
    );
    let rows = run(query_raw::<RawEnv, _, _>(sql.clone(), vec![]).provide_layer(layer())).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(sql.to_string(), "DELETE FROM audit_2026");
    assert!(
      dangerous_opt_outs()
        .iter()
        .any(|(id, _)| *id == "id_effect_sql::RawSqlCapability::layer_dangerous")
    );
    assert_eq!(test.recorded_executes()[0].0, "DELETE FROM audit_2026");
  }
}
