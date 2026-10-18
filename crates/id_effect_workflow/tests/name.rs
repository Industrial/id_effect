//! `WorkflowName` validation.

use id_effect_workflow::{WorkflowName, WorkflowNameError};

#[test]
fn accepts_key_safe_names() {
  for name in ["a", "close-account", "orders.v2", "X_1-y.2"] {
    assert_eq!(
      WorkflowName::new(name).map(|n| n.as_str().to_owned()),
      Ok(name.to_owned())
    );
  }
  assert!(WorkflowName::new("a".repeat(WorkflowName::MAX_LEN)).is_ok());
}

#[test]
fn rejects_empty_long_and_unsafe_names() {
  assert_eq!(WorkflowName::new(""), Err(WorkflowNameError::Empty));
  assert_eq!(
    WorkflowName::new("a".repeat(WorkflowName::MAX_LEN + 1)),
    Err(WorkflowNameError::TooLong {
      len: WorkflowName::MAX_LEN + 1
    })
  );
  assert_eq!(
    WorkflowName::new("close account"),
    Err(WorkflowNameError::InvalidChar { ch: ' ', index: 5 })
  );
  assert_eq!(
    WorkflowName::new("caf\u{e9}"),
    Err(WorkflowNameError::InvalidChar {
      ch: '\u{e9}',
      index: 3
    })
  );
  assert_eq!(
    WorkflowNameError::Empty.to_string(),
    "workflow name is empty"
  );
}
