//! [`UniqueViolation`]: recognising PostgreSQL's `unique_violation` in a [`SqlError`].

use id_effect_sql::SqlError;

/// PostgreSQL SQLSTATE 23505 (`unique_violation`), e.g. a second todo with a taken title.
///
/// `id_effect_sql` keeps only the driver's message, not the SQLSTATE, so the violation is
/// recognised by PostgreSQL's (English, default `lc_messages`) message text. This is the one
/// place that depends on it.
pub(crate) struct UniqueViolation;

impl UniqueViolation {
  /// The message PostgreSQL reports for SQLSTATE 23505.
  pub(crate) const MESSAGE: &'static str = "duplicate key value violates unique constraint";

  /// Whether `err` is a statement failing on a unique constraint.
  pub(crate) fn matches(err: &SqlError) -> bool {
    matches!(err, SqlError::QueryFailed { message, .. } if message.contains(Self::MESSAGE))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn failed(message: &str) -> SqlError {
    SqlError::QueryFailed {
      sql: "UPDATE todos ..".into(),
      message: message.into(),
    }
  }

  #[test]
  fn recognises_the_postgres_message() {
    let dup = failed(
      "error returned from database: duplicate key value violates unique constraint \
       \"todos_title_key\"",
    );
    assert!(UniqueViolation::matches(&dup));
  }

  #[test]
  fn other_failures_are_not_violations() {
    assert!(!UniqueViolation::matches(&failed(
      "relation \"todos\" does not exist"
    )));
    assert!(!UniqueViolation::matches(&SqlError::NotConnected));
    assert!(!UniqueViolation::matches(&SqlError::Decode(
      UniqueViolation::MESSAGE.into()
    )));
  }
}
