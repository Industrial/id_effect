//! `Topic` validation.

use id_effect_messaging::{Topic, TopicError};

#[test]
fn accepts_broker_safe_names() {
  for name in ["a", "ledger.entry-posted", "orders_v2", "X-1.y_2"] {
    assert_eq!(
      Topic::new(name).map(|t| t.as_str().to_owned()),
      Ok(name.to_owned())
    );
  }
  assert!(Topic::new("a".repeat(Topic::MAX_LEN)).is_ok());
}

#[test]
fn rejects_empty_long_and_unsafe_names() {
  assert_eq!(Topic::new(""), Err(TopicError::Empty));
  assert_eq!(
    Topic::new("a".repeat(Topic::MAX_LEN + 1)),
    Err(TopicError::TooLong {
      len: Topic::MAX_LEN + 1
    })
  );
  assert_eq!(
    Topic::new("orders/eu"),
    Err(TopicError::InvalidChar { ch: '/', index: 6 })
  );
  assert_eq!(
    Topic::new("caf\u{e9}"),
    Err(TopicError::InvalidChar {
      ch: '\u{e9}',
      index: 3
    })
  );
}
