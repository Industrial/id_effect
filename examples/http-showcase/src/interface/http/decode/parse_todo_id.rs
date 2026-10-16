//! [`parse_todo_id`]: the `{id}` path segment.

use crate::domain::todo::TodoId;
use crate::interface::http::ApiError;

/// `raw` as a [`TodoId`]: a decimal `i64` (`422 invalid` otherwise).
pub fn parse_todo_id(raw: &str) -> Result<TodoId, ApiError> {
  raw
    .parse()
    .map(TodoId::new)
    .map_err(|_| ApiError::invalid(format!("the todo id must be an integer, not {raw:?}")))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_integers_only() {
    assert_eq!(parse_todo_id("42"), Ok(TodoId::new(42)));
    let err = parse_todo_id("abc").unwrap_err();
    assert_eq!((err.status.as_u16(), err.code), (422, "invalid"));
    assert_eq!(err.message, "the todo id must be an integer, not \"abc\"");
    assert!(parse_todo_id("4.2").is_err());
  }
}
