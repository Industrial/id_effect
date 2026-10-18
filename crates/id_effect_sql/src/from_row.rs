//! Typed rows: [`FromRow`] decodes an [`SqlRow`] into a struct (`#[derive(FromRow)]`) or a tuple,
//! and [`query_as`] / [`query_one_as`] / [`query_opt_as`] run a query straight into them.
//!
//! ```rust
//! use id_effect_sql::{FromRow, SqlError, SqlRow};
//!
//! #[derive(Debug, PartialEq, FromRow)]
//! struct Todo {
//!   id: i64,
//!   #[from_row(rename = "title_text")]
//!   title: String,
//!   note: Option<String>,
//! }
//!
//! let row = SqlRow::from_pairs([
//!   ("id", 1i64.into()),
//!   ("title_text", "docs".into()),
//!   ("note", id_effect_sql::SqlValue::Null),
//! ]);
//! assert_eq!(Todo::from_row(&row), Ok(Todo { id: 1, title: "docs".into(), note: None }));
//! assert_eq!(<(i64, String)>::from_row(&row), Ok((1, "docs".into())));
//! assert_eq!(
//!   Todo::from_row(&SqlRow::from_pairs([("id", 1i64)])),
//!   Err(SqlError::ColumnNotFound { column: "title_text".into() })
//! );
//! ```

use id_effect::schema::HasSchema;
use id_effect::{Effect, HList, Selector, fail, succeed};

use crate::client::{SqlClientService, query};
use crate::error::SqlError;
use crate::raw::Sql;
use crate::row::{ColumnIndex, SqlRow};
use crate::value::{FromSqlValue, SqlValue};

/// A type decoded from one [`SqlRow`].
///
/// Derive it for a struct with named fields (`#[derive(FromRow)]`, see the derive's docs for
/// `rename`, `default`, `flatten` and `schema`); tuples of up to 12 [`FromSqlValue`] types decode
/// columns by position.
pub trait FromRow: Sized {
  /// Decode `row`; errors name the column ([`SqlError::ColumnNotFound`],
  /// [`SqlError::TypeMismatch`], …) or carry every schema issue ([`SqlError::InvalidRow`]).
  fn from_row(row: &SqlRow) -> Result<Self, SqlError>;
}

/// Decode `row` with `T`'s [`HasSchema`] schema over [`SqlRow::to_unknown`] (what
/// `#[from_row(schema)]` generates): refinements and brands run on database data, and every issue
/// the schema's accumulating decoder reports comes back in one [`SqlError::InvalidRow`].
pub fn from_row_with_schema<T>(row: &SqlRow) -> Result<T, SqlError>
where
  T: HasSchema<A = T>,
{
  T::schema()
    .decode_unknown_all(&row.to_unknown())
    .map_err(|e| SqlError::InvalidRow { issues: e.issues })
}

/// Column `idx` decoded as `T`, or `T::default()` when the row has no such column (what
/// `#[from_row(default)]` generates). Other errors are returned as they are.
pub fn get_or_default<T>(row: &SqlRow, idx: impl ColumnIndex) -> Result<T, SqlError>
where
  T: FromSqlValue + Default,
{
  match row.get::<T>(idx) {
    Err(SqlError::ColumnNotFound { .. }) => Ok(T::default()),
    other => other,
  }
}

impl FromRow for SqlRow {
  fn from_row(row: &SqlRow) -> Result<Self, SqlError> {
    Ok(row.clone())
  }
}

macro_rules! tuple_from_row {
  ($($t:ident $i:tt),+) => {
    impl<$($t: FromSqlValue),+> FromRow for ($($t,)+) {
      fn from_row(row: &SqlRow) -> Result<Self, SqlError> {
        Ok(($(row.get::<$t>({ const IDX: usize = $i; IDX })?,)+))
      }
    }
  };
}

tuple_from_row!(A 0);
tuple_from_row!(A 0, B 1);
tuple_from_row!(A 0, B 1, C 2);
tuple_from_row!(A 0, B 1, C 2, D 3);
tuple_from_row!(A 0, B 1, C 2, D 3, E 4);
tuple_from_row!(A 0, B 1, C 2, D 3, E 4, F 5);
tuple_from_row!(A 0, B 1, C 2, D 3, E 4, F 5, G 6);
tuple_from_row!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7);
tuple_from_row!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8);
tuple_from_row!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9);
tuple_from_row!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10);
tuple_from_row!(A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9, K 10, L 11);

/// Decode every row, stopping at the first failure.
pub fn from_rows<T: FromRow>(rows: &[SqlRow]) -> Result<Vec<T>, SqlError> {
  rows.iter().map(T::from_row).collect()
}

fn ready<A: Send + 'static, R: 'static>(r: Result<A, SqlError>) -> Effect<A, SqlError, R> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

/// [`query`] decoding every row as `T`: `query_as::<Todo, _, _>(sql, params![..])` where the
/// return type fixes the environment, else `query_as::<Todo, SqlClientEnv, _>(..)`
/// ([`SqlClientEnv`](crate::SqlClientEnv)).
pub fn query_as<T, R, I>(sql: impl Into<Sql>, params: Vec<SqlValue>) -> Effect<Vec<T>, SqlError, R>
where
  T: FromRow + Send + 'static,
  R: Selector<SqlClientService, I> + HList,
{
  query::<R, I>(sql, params).flat_map(|rows| ready(from_rows(&rows)))
}

/// [`query`] decoding the first row as `T`; [`SqlError::RowNotFound`] when there is none.
pub fn query_one_as<T, R, I>(sql: impl Into<Sql>, params: Vec<SqlValue>) -> Effect<T, SqlError, R>
where
  T: FromRow + Send + 'static,
  R: Selector<SqlClientService, I> + HList,
{
  query::<R, I>(sql, params).flat_map(|rows| {
    ready(
      rows
        .first()
        .ok_or(SqlError::RowNotFound)
        .and_then(T::from_row),
    )
  })
}

/// [`query`] decoding the first row as `T`, or `None` when there is none.
pub fn query_opt_as<T, R, I>(
  sql: impl Into<Sql>,
  params: Vec<SqlValue>,
) -> Effect<Option<T>, SqlError, R>
where
  T: FromRow + Send + 'static,
  R: Selector<SqlClientService, I> + HList,
{
  query::<R, I>(sql, params).flat_map(|rows| ready(rows.first().map(T::from_row).transpose()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::value::SqlType;

  #[test]
  fn tuples_decode_by_position() {
    let row = SqlRow::from_pairs([("a", 1i64), ("b", 2), ("c", 3)]);
    assert_eq!(<(i64,)>::from_row(&row), Ok((1,)));
    assert_eq!(<(i64, i32, i16)>::from_row(&row), Ok((1, 2, 3)));
    assert_eq!(
      <(i64, i64, i64, i64)>::from_row(&row),
      Err(SqlError::ColumnNotFound {
        column: "#3".into()
      })
    );
    assert_eq!(
      <(i64, bool)>::from_row(&row),
      Err(SqlError::TypeMismatch {
        column: "b".into(),
        expected: SqlType::Bool,
        found: SqlType::I64,
      })
    );
    let wide = SqlRow::from_pairs((0..12).map(|i| (format!("c{i}"), i64::from(i))));
    let all = <(i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64)>::from_row(&wide);
    assert_eq!(all.map(|t| t.11), Ok(11));
    assert_eq!(SqlRow::from_row(&wide).as_ref(), Ok(&wide));
  }

  #[test]
  fn get_or_default_only_defaults_missing_columns() {
    let row = SqlRow::from_pairs([("n", SqlValue::Null), ("t", "x".into())]);
    assert_eq!(get_or_default::<String>(&row, "gone"), Ok(String::new()));
    assert_eq!(get_or_default::<String>(&row, "t"), Ok("x".into()));
    assert_eq!(
      get_or_default::<i64>(&row, "n"),
      Err(SqlError::UnexpectedNull { column: "n".into() })
    );
  }

  #[test]
  fn from_rows_stops_at_the_first_failure() {
    let rows = [
      SqlRow::from_pairs([("a", 1i64)]),
      SqlRow::from_pairs([("a", 2i64)]),
    ];
    assert_eq!(from_rows::<(i64,)>(&rows), Ok(vec![(1,), (2,)]));
    assert!(from_rows::<(bool,)>(&rows).is_err());
  }
}
