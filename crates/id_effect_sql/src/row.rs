//! Typed result rows: [`SqlRow`] (named, typed columns of [`SqlValue`]s) and [`SqlColumn`].
//!
//! ```rust
//! use id_effect_sql::{SqlError, SqlRow, SqlValue};
//!
//! let row = SqlRow::from_pairs([("id", SqlValue::I64(7)), ("title", "docs".into())]);
//! assert_eq!(row.get::<i64>("id"), Ok(7));
//! assert_eq!(row.get::<String>(1), Ok("docs".into()));
//! assert_eq!(row.get::<Option<bool>>("missing").unwrap_err(),
//!   SqlError::ColumnNotFound { column: "missing".into() });
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use id_effect::schema::Unknown;

use crate::error::SqlError;
use crate::value::{FromSqlValue, SqlType, SqlValue, ToSqlValue};

/// One result column: its name, portable [`SqlType`], and the database's own type name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SqlColumn {
  name: Arc<str>,
  sql_type: SqlType,
  db_type: Arc<str>,
}

impl SqlColumn {
  /// A column named `name` of type `sql_type` (no database type name).
  pub fn new(name: impl Into<Arc<str>>, sql_type: SqlType) -> Self {
    Self {
      name: name.into(),
      sql_type,
      db_type: Arc::from(""),
    }
  }

  /// This column with the database's type name (e.g. `"INT4"`, `"jsonb"`, `"DATETIME"`).
  pub fn with_db_type(mut self, db_type: impl Into<Arc<str>>) -> Self {
    self.db_type = db_type.into();
    self
  }

  /// The column name (`?column?` and the like for unnamed expressions).
  pub fn name(&self) -> &str {
    &self.name
  }

  /// The portable type of the column's values.
  pub fn sql_type(&self) -> SqlType {
    self.sql_type
  }

  /// The database's type name (empty when unknown, e.g. for rows built in tests).
  pub fn db_type(&self) -> &str {
    &self.db_type
  }
}

mod sealed {
  pub trait Sealed {}
  impl Sealed for usize {}
  impl Sealed for &str {}
  impl Sealed for &String {}
}

/// A column index for [`SqlRow::get`] / [`SqlRow::value`]: a position (`usize`) or a name
/// (`&str`, first match, case-sensitive).
pub trait ColumnIndex: sealed::Sealed {
  /// The position of this column in `row`, or [`SqlError::ColumnNotFound`].
  fn position(&self, row: &SqlRow) -> Result<usize, SqlError>;
}

impl ColumnIndex for usize {
  fn position(&self, row: &SqlRow) -> Result<usize, SqlError> {
    if *self < row.values.len() {
      Ok(*self)
    } else {
      Err(SqlError::ColumnNotFound {
        column: format!("#{self}"),
      })
    }
  }
}

impl ColumnIndex for &str {
  fn position(&self, row: &SqlRow) -> Result<usize, SqlError> {
    row
      .columns
      .iter()
      .position(|c| c.name() == *self)
      .ok_or_else(|| SqlError::ColumnNotFound {
        column: (*self).to_owned(),
      })
  }
}

impl ColumnIndex for &String {
  fn position(&self, row: &SqlRow) -> Result<usize, SqlError> {
    self.as_str().position(row)
  }
}

/// One result row: typed values under named columns. The columns are shared by all rows of a
/// result set.
///
/// Read a column with [`get`](Self::get) (`row.get::<i64>("id")`, `row.get::<String>(1)`); it
/// returns a [`SqlError`] naming the column and never panics. Build rows for
/// [`TestSqlClient`](crate::TestSqlClient) scripts with [`from_pairs`](Self::from_pairs).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SqlRow {
  columns: Arc<[SqlColumn]>,
  values: Vec<SqlValue>,
}

impl SqlRow {
  /// A row of `values` under `columns`; fails with [`SqlError::Decode`] when their lengths differ.
  pub fn from_parts(columns: Arc<[SqlColumn]>, values: Vec<SqlValue>) -> Result<Self, SqlError> {
    if columns.len() != values.len() {
      return Err(SqlError::Decode(format!(
        "row has {} columns but {} values",
        columns.len(),
        values.len()
      )));
    }
    Ok(Self { columns, values })
  }

  /// A row of `(column name, value)` pairs; each column's type is its value's type.
  pub fn from_pairs<I, N, V>(pairs: I) -> Self
  where
    I: IntoIterator<Item = (N, V)>,
    N: Into<Arc<str>>,
    V: ToSqlValue,
  {
    let (columns, values): (Vec<SqlColumn>, Vec<SqlValue>) = pairs
      .into_iter()
      .map(|(name, v)| {
        let v = v.to_sql_value();
        (SqlColumn::new(name, v.sql_type()), v)
      })
      .unzip();
    Self {
      columns: columns.into(),
      values,
    }
  }

  /// The columns, left to right.
  pub fn columns(&self) -> &[SqlColumn] {
    &self.columns
  }

  /// The values, left to right.
  pub fn values(&self) -> &[SqlValue] {
    &self.values
  }

  /// The values, left to right.
  pub fn into_values(self) -> Vec<SqlValue> {
    self.values
  }

  /// The number of columns.
  pub fn len(&self) -> usize {
    self.values.len()
  }

  /// Whether the row has no columns.
  pub fn is_empty(&self) -> bool {
    self.values.is_empty()
  }

  /// The value of column `idx`, or [`SqlError::ColumnNotFound`].
  pub fn value(&self, idx: impl ColumnIndex) -> Result<&SqlValue, SqlError> {
    let i = idx.position(self)?;
    Ok(&self.values[i])
  }

  /// Column `idx` decoded as `T` ([`FromSqlValue`]). Errors name the column:
  /// [`SqlError::ColumnNotFound`], [`SqlError::UnexpectedNull`], [`SqlError::TypeMismatch`],
  /// [`SqlError::ValueOutOfRange`], or [`SqlError::Decode`] for a malformed value.
  pub fn get<T: FromSqlValue>(&self, idx: impl ColumnIndex) -> Result<T, SqlError> {
    let i = idx.position(self)?;
    T::from_sql_value(&self.values[i]).map_err(|e| SqlError::from_value_error(self.label(i), e))
  }

  /// The row as an [`Unknown::Object`] keyed by column name (the first of duplicate names
  /// wins), each value as [`SqlValue::to_unknown`]: the input of a `Schema` decoder.
  pub fn to_unknown(&self) -> Unknown {
    let mut fields = BTreeMap::new();
    for (column, value) in self.columns.iter().zip(&self.values) {
      fields
        .entry(column.name().to_owned())
        .or_insert_with(|| value.to_unknown());
    }
    Unknown::Object(fields)
  }

  /// The name of column `i` for error messages: its name, or `#i` when it has none.
  fn label(&self, i: usize) -> String {
    match self.columns.get(i).map(SqlColumn::name) {
      Some(name) if !name.is_empty() => name.to_owned(),
      _ => format!("#{i}"),
    }
  }

  /// A row of text cells under columns `#0`, `#1`, … (the 0.8 constructor).
  #[deprecated(since = "0.9.0", note = "build rows with SqlRow::from_pairs")]
  pub fn new(cells: Vec<String>) -> Self {
    Self::from_pairs(
      cells
        .into_iter()
        .enumerate()
        .map(|(i, cell)| (format!("#{i}"), SqlValue::Text(cell))),
    )
  }

  /// The values rendered as 0.8 text cells: NULL is empty, numbers and booleans are their
  /// `to_string`, bytes are `{:?}`, uuids hyphenated, dates and times ISO 8601, JSON compact.
  #[deprecated(since = "0.9.0", note = "use SqlRow::get::<T>(idx) or value(idx)")]
  pub fn text_cells(&self) -> Vec<String> {
    self.values.iter().map(SqlValue::render).collect()
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::sync::Arc;

  use id_effect::schema::Unknown;

  use super::{SqlColumn, SqlRow};
  use crate::error::SqlError;
  use crate::value::{SqlType, SqlValue};

  fn row() -> SqlRow {
    SqlRow::from_pairs([
      ("id", SqlValue::I64(7)),
      ("title", SqlValue::Text("docs".into())),
      ("done", SqlValue::Null),
      ("id", SqlValue::I64(8)),
    ])
  }

  #[test]
  fn get_by_name_equals_get_by_index() {
    let row = row();
    assert_eq!(row.get::<i64>("id"), row.get::<i64>(0usize));
    assert_eq!(row.get::<i64>("id"), Ok(7), "first of duplicate names");
    let title = String::from("title");
    assert_eq!(row.get::<String>(&title), Ok("docs".into()));
    assert_eq!(row.get::<Option<bool>>("done"), Ok(None));
    assert_eq!(row.value(3usize), Ok(&SqlValue::I64(8)));
    assert_eq!((row.len(), row.is_empty()), (4, false));
    assert!(SqlRow::default().is_empty());
    assert_eq!(row.columns()[1].sql_type(), SqlType::Text);
    assert_eq!(row.values()[0], SqlValue::I64(7));
    assert_eq!(row.clone().into_values().len(), 4);
  }

  #[test]
  fn missing_columns_are_errors_not_panics() {
    let row = row();
    assert_eq!(
      row.get::<i64>("nope"),
      Err(SqlError::ColumnNotFound {
        column: "nope".into()
      })
    );
    assert_eq!(
      row.value(9usize),
      Err(SqlError::ColumnNotFound {
        column: "#9".into()
      })
    );
    assert!(row.value("ID").is_err(), "names are case-sensitive");
  }

  #[test]
  fn decode_error_names_the_column() {
    let row = row();
    assert_eq!(
      row.get::<bool>("done"),
      Err(SqlError::UnexpectedNull {
        column: "done".into()
      })
    );
    assert_eq!(
      row.get::<i64>("title"),
      Err(SqlError::TypeMismatch {
        column: "title".into(),
        expected: SqlType::I64,
        found: SqlType::Text,
      })
    );
    let wide = SqlRow::from_pairs([("n", i64::MAX)]);
    assert!(matches!(
      wide.get::<i32>("n"),
      Err(SqlError::ValueOutOfRange { column, .. }) if column == "n"
    ));
    let unnamed = SqlRow::from_parts(
      Arc::from([SqlColumn::new("", SqlType::Text)]),
      vec![SqlValue::Text("x".into())],
    )
    .unwrap();
    assert!(matches!(
      unnamed.get::<uuid::Uuid>(0usize),
      Err(SqlError::Decode(m)) if m.starts_with("column `#0`")
    ));
  }

  #[test]
  fn from_parts_checks_lengths() {
    let columns: Arc<[SqlColumn]> =
      Arc::from([SqlColumn::new("a", SqlType::I32).with_db_type("INT4")]);
    assert_eq!(columns[0].db_type(), "INT4");
    assert_eq!(columns[0].name(), "a");
    assert!(matches!(
      SqlRow::from_parts(Arc::clone(&columns), vec![]),
      Err(SqlError::Decode(_))
    ));
    let row = SqlRow::from_parts(columns, vec![SqlValue::I32(1)]).unwrap();
    assert_eq!(row.get::<i32>("a"), Ok(1));
    assert_eq!(row.columns()[0].db_type(), "INT4");
    assert_eq!(SqlColumn::new("b", SqlType::Json).db_type(), "");
  }

  #[test]
  fn to_unknown_is_an_object_keyed_by_name() {
    assert_eq!(
      row().to_unknown(),
      Unknown::Object(BTreeMap::from([
        ("id".to_owned(), Unknown::I64(7)),
        ("title".to_owned(), Unknown::String("docs".into())),
        ("done".to_owned(), Unknown::Null),
      ]))
    );
  }

  #[test]
  #[allow(deprecated)]
  fn legacy_new_and_text_cells_keep_the_0_8_strings() {
    let legacy = SqlRow::new(vec!["1".into(), String::new()]);
    assert_eq!(legacy.text_cells(), ["1", ""]);
    assert_eq!(legacy.columns()[1].name(), "#1");
    assert_eq!(legacy.get::<String>(0usize), Ok("1".into()));
    let typed = SqlRow::from_pairs([
      ("n", SqlValue::Null),
      ("b", SqlValue::Bool(false)),
      ("i", SqlValue::I32(42)),
      ("f", SqlValue::F64(3.25)),
      ("bytes", SqlValue::Bytes(vec![1, 2, 3])),
    ]);
    assert_eq!(typed.text_cells(), ["", "false", "42", "3.25", "[1, 2, 3]"]);
  }
}
