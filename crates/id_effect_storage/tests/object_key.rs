//! `ObjectKey` validation.

use id_effect_storage::{KeyError, ObjectKey};

#[test]
fn accepts_relative_slash_separated_keys() {
  let key = ObjectKey::new("invoices/2026/10/inv-0042.pdf").unwrap();
  assert_eq!(key.as_str(), "invoices/2026/10/inv-0042.pdf");
  assert_eq!(key.segments().count(), 4);
  assert_eq!(key.file_name(), "inv-0042.pdf");
  assert_eq!(ObjectKey::new("readme").unwrap().file_name(), "readme");
  assert!(ObjectKey::new("a".repeat(ObjectKey::MAX_LEN)).is_ok());
  assert!(ObjectKey::new("...hidden/..x").is_ok());
}

#[test]
fn rejects_keys_that_differ_between_backends() {
  let cases = [
    ("", KeyError::Empty),
    ("/abs", KeyError::EmptySegment),
    ("dir/", KeyError::EmptySegment),
    ("a//b", KeyError::EmptySegment),
    ("./a", KeyError::DotSegment),
    ("a/../../b", KeyError::DotSegment),
    ("a\nb", KeyError::ControlChar),
  ];
  for (key, error) in cases {
    assert_eq!(ObjectKey::new(key), Err(error), "{key:?}");
  }
  assert_eq!(
    ObjectKey::new("a".repeat(ObjectKey::MAX_LEN + 1)),
    Err(KeyError::TooLong {
      len: ObjectKey::MAX_LEN + 1
    })
  );
}
