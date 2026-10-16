//! [`TodoRow`]: a `todos` row (`id, title, completed`) as returned by the driver.

use id_effect_sql::SqlRow;

use crate::domain::todo::{StoreError, Title, Todo, TodoId};

/// The cells of one `id, title, completed` row, parsed but not yet validated as a [`Todo`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TodoRow {
  id: i64,
  title: String,
  completed: bool,
}

impl TodoRow {
  /// Parses the text cells of `row`: an integer id, any title, `true` / `false`.
  pub(crate) fn parse(row: &SqlRow) -> Result<Self, StoreError> {
    let bad = || StoreError::Internal(format!("unexpected todo row {:?}", row.cells));
    let [id, title, completed] = row.cells.as_slice() else {
      return Err(bad());
    };
    Ok(Self {
      id: id.parse().map_err(|_| bad())?,
      title: title.clone(),
      completed: match completed.as_str() {
        "true" => true,
        "false" => false,
        _ => return Err(bad()),
      },
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
  use super::*;

  fn row(cells: &[&str]) -> SqlRow {
    SqlRow::new(cells.iter().map(|c| (*c).to_owned()).collect())
  }

  #[test]
  fn decodes_a_well_formed_row() {
    let todo = TodoRow::decode(&row(&["3", "t", "true"])).unwrap();
    assert_eq!(
      todo,
      Todo::restore(TodoId::new(3), Title::parse("t").unwrap(), true)
    );
    assert!(
      !TodoRow::decode(&row(&["3", "t", "false"]))
        .unwrap()
        .is_completed()
    );
  }

  #[test]
  fn rejects_malformed_rows() {
    for bad in [
      &["x", "t", "true"][..],
      &["3", "t", "yes"],
      &["3", "t"],
      &[],
    ] {
      assert!(
        matches!(TodoRow::decode(&row(bad)), Err(StoreError::Internal(_))),
        "{bad:?}"
      );
    }
  }

  #[test]
  fn rejects_a_stored_title_that_is_not_a_title() {
    for title in ["", "  ", " padded "] {
      assert!(
        matches!(
          TodoRow::decode(&row(&["1", title, "false"])),
          Err(StoreError::Internal(_))
        ),
        "{title:?}"
      );
    }
    let long = "x".repeat(Title::MAX_CHARS + 1);
    assert!(TodoRow::decode(&row(&["1", &long, "false"])).is_err());
  }

  #[test]
  fn decodes_result_sets() {
    let rows = [row(&["1", "a", "false"]), row(&["2", "b", "true"])];
    let all = TodoRow::decode_all(&rows).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(TodoRow::decode_first(&rows).unwrap(), Some(all[0].clone()));
    assert_eq!(TodoRow::decode_first(&[]).unwrap(), None);
    assert!(TodoRow::decode_all(&[row(&["1"])]).is_err());
  }
}
