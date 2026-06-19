//! JSON Patch ([RFC 6902](https://datatracker.ietf.org/doc/html/rfc6902)) for [`Unknown`](id_effect::schema::Unknown).

use crate::schema_bridge::{SchemaPathError, create_at_path, get_at_path, set_at_path};
use id_effect::schema::Unknown;
use thiserror::Error;

/// Supported patch operations.
#[derive(Clone, Debug, PartialEq)]
pub enum PatchOp {
  /// Insert or create a value at `path`.
  Add {
    /// Dot-separated or JSON Pointer path.
    path: String,
    /// Value to insert.
    value: Unknown,
  },
  /// Replace an existing value at `path`.
  Replace {
    /// Target path.
    path: String,
    /// Replacement value.
    value: Unknown,
  },
  /// Remove the value at `path`.
  Remove {
    /// Target path.
    path: String,
  },
  /// Move a value from `from` to `path`.
  Move {
    /// Source path.
    from: String,
    /// Destination path.
    path: String,
  },
  /// Copy a value from `from` to `path`.
  Copy {
    /// Source path.
    from: String,
    /// Destination path.
    path: String,
  },
  /// Assert a value at `path` equals `value`.
  Test {
    /// Target path.
    path: String,
    /// Expected value.
    value: Unknown,
  },
}

/// Patch application failure.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum PatchError {
  /// Path resolution failed.
  #[error(transparent)]
  Path(#[from] SchemaPathError),
  /// Remove/replace/test targeted a missing path or mismatched value.
  #[error("patch path {path}: {message}")]
  Missing {
    /// Path that failed.
    path: String,
    /// Reason.
    message: String,
  },
}

/// Apply a single patch operation to a document.
pub fn apply_patch(doc: Unknown, op: &PatchOp) -> Result<Unknown, PatchError> {
  match op {
    PatchOp::Add { path, value } => apply_add(doc, path, value.clone()),
    PatchOp::Replace { path, value } => apply_replace(doc, path, value.clone()),
    PatchOp::Remove { path } => apply_remove(doc, path),
    PatchOp::Move { from, path } => apply_move(doc, from, path),
    PatchOp::Copy { from, path } => apply_copy(doc, from, path),
    PatchOp::Test { path, value } => apply_test(doc, path, value),
  }
}

/// Apply a sequence of patch operations in order.
pub fn apply_patches(doc: Unknown, ops: &[PatchOp]) -> Result<Unknown, PatchError> {
  ops.iter().try_fold(doc, apply_patch)
}

fn apply_add(doc: Unknown, path: &str, value: Unknown) -> Result<Unknown, PatchError> {
  if path.is_empty() {
    return Ok(value);
  }
  if get_at_path(&doc, path).is_ok() {
    return apply_replace(doc, path, value);
  }
  create_at_path(doc, path, value).map_err(PatchError::from)
}

fn apply_replace(doc: Unknown, path: &str, value: Unknown) -> Result<Unknown, PatchError> {
  get_at_path(&doc, path).map_err(PatchError::from)?;
  set_at_path(doc, path, value).map_err(PatchError::from)
}

fn apply_remove(doc: Unknown, path: &str) -> Result<Unknown, PatchError> {
  if path.is_empty() {
    return Err(PatchError::Missing {
      path: path.to_string(),
      message: "cannot remove document root".into(),
    });
  }
  let segments = crate::path::parse_path(path).map_err(PatchError::from)?;
  if segments.is_empty() {
    return Err(PatchError::Missing {
      path: path.to_string(),
      message: "cannot remove document root".into(),
    });
  }
  let parent_segments = &segments[..segments.len() - 1];
  let last = segments.last().expect("non-empty segments");
  let parent_path = crate::path::segments_to_dot(parent_segments);

  let parent = if parent_segments.is_empty() {
    doc.clone()
  } else {
    get_at_path(&doc, &parent_path)
      .map_err(PatchError::from)?
      .clone()
  };

  let updated_parent = match (parent, last) {
    (Unknown::Object(mut map), crate::path::PathSegment::Field(name)) => {
      if map.remove(name).is_none() {
        return Err(PatchError::Missing {
          path: path.to_string(),
          message: format!("missing field `{name}`"),
        });
      }
      Unknown::Object(map)
    }
    (Unknown::Array(mut items), crate::path::PathSegment::Index(index)) => {
      if *index >= items.len() {
        return Err(PatchError::Missing {
          path: path.to_string(),
          message: format!("index {index} out of bounds"),
        });
      }
      items.remove(*index);
      Unknown::Array(items)
    }
    (other, segment) => {
      return Err(PatchError::Missing {
        path: path.to_string(),
        message: format!("cannot remove {segment:?} from {other:?}"),
      });
    }
  };

  if parent_segments.is_empty() {
    Ok(updated_parent)
  } else {
    set_at_path(doc, &parent_path, updated_parent).map_err(PatchError::from)
  }
}

fn apply_move(doc: Unknown, from: &str, path: &str) -> Result<Unknown, PatchError> {
  let value = get_at_path(&doc, from).map_err(PatchError::from)?.clone();
  let without = apply_remove(doc, from)?;
  apply_add(without, path, value)
}

fn apply_copy(doc: Unknown, from: &str, path: &str) -> Result<Unknown, PatchError> {
  let value = get_at_path(&doc, from).map_err(PatchError::from)?.clone();
  apply_add(doc, path, value)
}

fn apply_test(doc: Unknown, path: &str, value: &Unknown) -> Result<Unknown, PatchError> {
  let current = get_at_path(&doc, path).map_err(PatchError::from)?;
  if current != value {
    return Err(PatchError::Missing {
      path: path.to_string(),
      message: "test value mismatch".into(),
    });
  }
  Ok(doc)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::schema_bridge::object;
  use id_effect::schema::Unknown;

  fn doc() -> Unknown {
    object([
      ("count", Unknown::I64(1)),
      ("name", Unknown::String("Ada".into())),
    ])
  }

  mod apply_patch {
    use super::*;

    #[test]
    fn replace_updates_existing_field() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Replace {
          path: "name".into(),
          value: Unknown::String("Grace".into()),
        },
      )
      .unwrap();
      assert_eq!(
        get_at_path(&updated, "name").unwrap(),
        &Unknown::String("Grace".into())
      );
    }

    #[test]
    fn add_inserts_new_field() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Add {
          path: "extra".into(),
          value: Unknown::Bool(true),
        },
      )
      .unwrap();
      assert_eq!(
        get_at_path(&updated, "extra").unwrap(),
        &Unknown::Bool(true)
      );
    }

    #[test]
    fn add_creates_nested_path() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Add {
          path: "b.c".into(),
          value: Unknown::I64(2),
        },
      )
      .unwrap();
      assert_eq!(get_at_path(&updated, "b.c").unwrap(), &Unknown::I64(2));
    }

    #[test]
    fn remove_deletes_field() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Remove {
          path: "name".into(),
        },
      )
      .unwrap();
      assert!(get_at_path(&updated, "name").is_err());
    }

    #[test]
    fn replace_errors_when_missing() {
      let err = apply_patch(
        doc(),
        &PatchOp::Replace {
          path: "missing".into(),
          value: Unknown::Null,
        },
      )
      .unwrap_err();
      assert!(matches!(err, PatchError::Path(_)));
    }

    #[test]
    fn add_replaces_existing_field() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Add {
          path: "name".into(),
          value: Unknown::String("New".into()),
        },
      )
      .unwrap();
      assert_eq!(
        get_at_path(&updated, "name").unwrap(),
        &Unknown::String("New".into())
      );
    }

    #[test]
    fn add_empty_path_replaces_document() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Add {
          path: String::new(),
          value: Unknown::Bool(false),
        },
      )
      .unwrap();
      assert_eq!(updated, Unknown::Bool(false));
    }

    #[test]
    fn remove_root_is_rejected() {
      let err = apply_patch(
        doc(),
        &PatchOp::Remove {
          path: String::new(),
        },
      )
      .unwrap_err();
      assert!(matches!(err, PatchError::Missing { .. }));
    }

    #[test]
    fn remove_missing_field_errors() {
      let err = apply_patch(
        doc(),
        &PatchOp::Remove {
          path: "missing".into(),
        },
      )
      .unwrap_err();
      assert!(matches!(err, PatchError::Missing { .. }));
    }

    #[test]
    fn remove_array_element_by_index() {
      let arr = Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)]);
      let updated = apply_patch(arr, &PatchOp::Remove { path: "0".into() }).unwrap();
      assert_eq!(updated, Unknown::Array(vec![Unknown::I64(2)]));
    }

    #[test]
    fn remove_invalid_array_index_errors() {
      let arr = Unknown::Array(vec![Unknown::I64(1)]);
      let err = apply_patch(
        arr,
        &PatchOp::Remove {
          path: "nope".into(),
        },
      )
      .unwrap_err();
      assert!(matches!(err, PatchError::Missing { .. }));
    }

    #[test]
    fn remove_out_of_bounds_index_errors() {
      let arr = Unknown::Array(vec![Unknown::I64(1)]);
      let err = apply_patch(arr, &PatchOp::Remove { path: "5".into() }).unwrap_err();
      assert!(matches!(err, PatchError::Missing { .. }));
    }

    #[test]
    fn remove_nested_array_element() {
      let doc = object([(
        "items",
        Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)]),
      )]);
      let updated = apply_patch(
        doc,
        &PatchOp::Remove {
          path: "items.0".into(),
        },
      )
      .unwrap();
      assert_eq!(
        get_at_path(&updated, "items").unwrap(),
        &Unknown::Array(vec![Unknown::I64(2)])
      );
    }

    #[test]
    fn remove_from_scalar_errors() {
      let err =
        apply_patch(Unknown::Bool(true), &PatchOp::Remove { path: "0".into() }).unwrap_err();
      assert!(matches!(err, PatchError::Missing { .. }));
    }

    #[test]
    fn move_value_between_paths() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Move {
          from: "name".into(),
          path: "alias".into(),
        },
      )
      .unwrap();
      assert_eq!(
        get_at_path(&updated, "alias").unwrap(),
        &Unknown::String("Ada".into())
      );
      assert!(get_at_path(&updated, "name").is_err());
    }

    #[test]
    fn copy_value_to_new_path() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Copy {
          from: "name".into(),
          path: "alias".into(),
        },
      )
      .unwrap();
      assert_eq!(
        get_at_path(&updated, "alias").unwrap(),
        &Unknown::String("Ada".into())
      );
      assert_eq!(
        get_at_path(&updated, "name").unwrap(),
        &Unknown::String("Ada".into())
      );
    }

    #[test]
    fn test_passes_on_matching_value() {
      let updated = apply_patch(
        doc(),
        &PatchOp::Test {
          path: "count".into(),
          value: Unknown::I64(1),
        },
      )
      .unwrap();
      assert_eq!(get_at_path(&updated, "count").unwrap(), &Unknown::I64(1));
    }

    #[test]
    fn test_errors_on_mismatch() {
      let err = apply_patch(
        doc(),
        &PatchOp::Test {
          path: "count".into(),
          value: Unknown::I64(9),
        },
      )
      .unwrap_err();
      assert!(matches!(err, PatchError::Missing { .. }));
    }

    #[test]
    fn add_appends_array_element() {
      let arr = Unknown::Array(vec![Unknown::I64(1)]);
      let updated = apply_patch(
        arr,
        &PatchOp::Add {
          path: "-".into(),
          value: Unknown::I64(2),
        },
      )
      .unwrap();
      assert_eq!(
        updated,
        Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)])
      );
    }
  }

  mod apply_patches {
    use super::*;

    #[test]
    fn applies_operations_in_order() {
      let updated = apply_patches(
        doc(),
        &[
          PatchOp::Replace {
            path: "count".into(),
            value: Unknown::I64(2),
          },
          PatchOp::Add {
            path: "flag".into(),
            value: Unknown::Bool(true),
          },
        ],
      )
      .unwrap();
      assert_eq!(get_at_path(&updated, "count").unwrap(), &Unknown::I64(2));
      assert_eq!(get_at_path(&updated, "flag").unwrap(), &Unknown::Bool(true));
    }
  }
}
