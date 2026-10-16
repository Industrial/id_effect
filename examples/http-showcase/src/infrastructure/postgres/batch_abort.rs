//! [`BatchAbort`]: why the transaction of
//! [`PgTodoRepository::add_all`](super::PgTodoRepository) is rolled back.

use id_effect::{Effect, fail, succeed};
use id_effect_sql::SqlError;

use crate::domain::todo::{StoreError, TitleTaken, Todo};

/// The failure that aborts a batch insert's transaction. A taken title is a domain outcome,
/// but it must still *fail* the transaction bracket so that every earlier insert rolls back;
/// [`Self::settle`] turns it back into a success value outside the bracket.
#[derive(Clone, Debug)]
pub(crate) enum BatchAbort {
  /// A title of the batch is taken (by a stored todo or an earlier title of the batch).
  Taken(TitleTaken),
  /// The store failed.
  Store(StoreError),
}

impl BatchAbort {
  /// The port's outcome of a batch run in a transaction: a taken title is `Ok(Err(..))`, a
  /// store failure is the effect's failure.
  pub(crate) fn settle(
    batch: Effect<Vec<Todo>, BatchAbort>,
  ) -> Effect<Result<Vec<Todo>, TitleTaken>, StoreError> {
    batch.map(Ok).catch(|abort| match abort {
      Self::Taken(taken) => succeed(Err(taken)),
      Self::Store(err) => fail(err),
    })
  }
}

/// `BEGIN`, a statement or `COMMIT` failed ([`StoreError::from`]).
impl From<SqlError> for BatchAbort {
  fn from(err: SqlError) -> Self {
    Self::Store(StoreError::from(err))
  }
}

impl From<StoreError> for BatchAbort {
  fn from(err: StoreError) -> Self {
    Self::Store(err)
  }
}

#[cfg(test)]
mod tests {
  use id_effect::Exit;

  use super::*;
  use crate::domain::todo::Title;

  #[test]
  fn settle_keeps_a_taken_title_as_a_value() {
    let taken = TitleTaken(Title::parse("a").unwrap());
    let settled = BatchAbort::settle(fail(BatchAbort::Taken(taken.clone()))).block_on();
    assert!(matches!(settled, Exit::Success(Err(t)) if t == taken));
    let ok = BatchAbort::settle(succeed(Vec::new())).block_on();
    assert!(matches!(ok, Exit::Success(Ok(v)) if v.is_empty()));
  }

  #[test]
  fn settle_fails_with_store_errors() {
    let failed = BatchAbort::settle(fail(BatchAbort::from(SqlError::NotConnected))).block_on();
    assert!(matches!(
      failed,
      Exit::Failure(id_effect::Cause::Fail(StoreError::Unavailable(_)))
    ));
  }
}
