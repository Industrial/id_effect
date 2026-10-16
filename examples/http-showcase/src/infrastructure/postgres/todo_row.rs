//! [`TodoRow`]: a `todos` row (`id, title, completed`) as returned by the driver.

use id_effect_sql::SqlRow;

use crate::domain::todo::{StoreError, Title, Todo, TodoId};

/// The columns of one `id, title, completed` row, decoded but not yet validated as a [`Todo`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TodoRow {
  id: i64,
  title: String,
  completed: bool,
}

impl TodoRow {
  /// Decodes the columns of `row` by name: an integer `id`, a text `title`, a boolean
  /// `completed`. A missing, NULL or mistyped column is [`StoreError::Internal`] naming the
  /// column (`From<SqlError>`).
  pub(crate) fn parse(row: &SqlRow) -> Result<Self, StoreError> {
    Ok(Self {
      id: row.get("id")?,
      title: row.get("title")?,
      completed: row.get("completed")?,
    })
  }

  /// The [`Todo`] this row stores. A stored title that is not a valid [`Title`] (or not in its
  /// canonical, trimmed form) is corrupt data: [`StoreError::Internal`].
  pub(crate) fn into_todo(self) -> Result<Todo, StoreError> {
    let corrupt = || StoreError::Internal(format!("todo {} has an invalid title", self.id));
    let title = Title::parse(&self.title).map_err(|_| corrupt())?;
    if title.as_str() != self.title {
      return Err(corrupt());
    }
    Ok(Todo::restore(TodoId::new(self.id), title, self.completed))
  }

  /// [`Self::parse`] then [`Self::into_todo`].
  pub(crate) fn decode(row: &SqlRow) -> Result<Todo, StoreError> {
    Self::parse(row)?.into_todo()
  }

  /// Every row of a result set, decoded.
  pub(crate) fn decode_all(rows: &[SqlRow]) -> Result<Vec<Todo>, StoreError> {
    rows.iter().map(Self::decode).collect()
  }

  /// The first row of a result set, decoded; `None` when the statement returned no row.
  pub(crate) fn decode_first(rows: &[SqlRow]) -> Result<Option<Todo>, StoreError> {
    rows.first().map(Self::decode).transpose()
  }
}

#[cfg(test)]
mod tests {
  use id_effect_sql::SqlValue;

  use super::*;

  fn row(id: i64, title: &str, completed: bool) -> SqlRow {
    columns(id.into(), title.into(), completed.into())
  }

  fn columns(id: SqlValue, title: SqlValue, completed: SqlValue) -> SqlRow {
    SqlRow::from_pairs([("id", id), ("title", title), ("completed", completed)])
  }

  #[test]
  fn decodes_a_well_formed_row() {
    let todo = TodoRow::decode(&row(3, "t", true)).unwrap();
    assert_eq!(
      todo,
      Todo::restore(TodoId::new(3), Title::parse("t").unwrap(), true)
    );
    assert!(!TodoRow::decode(&row(3, "t", false)).unwrap().is_completed());
  }

  #[test]
  fn rejects_malformed_rows() {
    for bad in [
      columns("x".into(), "t".into(), true.into()),
      columns(3i64.into(), "t".into(), "yes".into()),
      columns(3i64.into(), SqlValue::Null, false.into()),
      SqlRow::from_pairs([("id", 3i64)]),
      SqlRow::default(),
    ] {
      assert!(
        matches!(TodoRow::decode(&bad), Err(StoreError::Internal(_))),
        "{bad:?}"
      );
    }
  }

  #[test]
  fn rejects_a_stored_title_that_is_not_a_title() {
    for title in ["", "  ", " padded "] {
      assert!(
        matches!(
          TodoRow::decode(&row(1, title, false)),
          Err(StoreError::Internal(_))
        ),
        "{title:?}"
      );
    }
    let long = "x".repeat(Title::MAX_CHARS + 1);
    assert!(TodoRow::decode(&row(1, &long, false)).is_err());
  }

  #[test]
  fn decodes_result_sets() {
    let rows = [row(1, "a", false), row(2, "b", true)];
    let all = TodoRow::decode_all(&rows).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(TodoRow::decode_first(&rows).unwrap(), Some(all[0].clone()));
    assert_eq!(TodoRow::decode_first(&[]).unwrap(), None);
    assert!(TodoRow::decode_all(&[SqlRow::from_pairs([("id", 1i64)])]).is_err());
  }
}
