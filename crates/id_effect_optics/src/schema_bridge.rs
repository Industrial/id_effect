//! Field-path access for [`id_effect::schema::Unknown`] values.

use id_effect::schema::Unknown;
use std::collections::BTreeMap;
use thiserror::Error;

/// Error when a field path cannot be resolved.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("schema path {path}: {message}")]
pub struct SchemaPathError {
  /// Dot-separated path attempted.
  pub path: String,
  /// Human-readable reason.
  pub message: String,
}

impl SchemaPathError {
  fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      path: path.into(),
      message: message.into(),
    }
  }
}

/// Read a nested field from [`Unknown`] using dot-separated segments.
pub fn get_at_path<'a>(value: &'a Unknown, path: &str) -> Result<&'a Unknown, SchemaPathError> {
  if path.is_empty() {
    return Ok(value);
  }
  let mut current = value;
  for (idx, segment) in path.split('.').enumerate() {
    let partial = path.split('.').take(idx + 1).collect::<Vec<_>>().join(".");
    current = match current {
      Unknown::Object(map) => map
        .get(segment)
        .ok_or_else(|| SchemaPathError::new(partial, format!("missing field `{segment}`")))?,
      Unknown::Array(items) => {
        let index: usize = segment.parse().map_err(|_| {
          SchemaPathError::new(partial.clone(), format!("invalid index `{segment}`"))
        })?;
        items
          .get(index)
          .ok_or_else(|| SchemaPathError::new(partial, format!("index {index} out of bounds")))?
      }
      other => {
        return Err(SchemaPathError::new(
          partial,
          format!("cannot traverse {other:?}"),
        ));
      }
    };
  }
  Ok(current)
}

/// Set (replace) a nested field, cloning the tree along the path.
pub fn set_at_path(
  value: Unknown,
  path: &str,
  new_value: Unknown,
) -> Result<Unknown, SchemaPathError> {
  if path.is_empty() {
    return Ok(new_value);
  }
  let mut segments: Vec<&str> = path.split('.').collect();
  let last = segments.pop().expect("non-empty path");
  let parent_path = segments.join(".");

  let parent = if parent_path.is_empty() {
    value.clone()
  } else {
    get_at_path(&value, &parent_path)?.clone()
  };

  let updated_parent = match parent {
    Unknown::Object(mut map) => {
      map.insert(last.to_string(), new_value);
      Unknown::Object(map)
    }
    Unknown::Array(mut items) => {
      let index: usize = last
        .parse()
        .map_err(|_| SchemaPathError::new(path.to_string(), format!("invalid index `{last}`")))?;
      if index >= items.len() {
        return Err(SchemaPathError::new(
          path.to_string(),
          format!("index {index} out of bounds"),
        ));
      }
      items[index] = new_value;
      Unknown::Array(items)
    }
    other => {
      return Err(SchemaPathError::new(
        path.to_string(),
        format!("cannot set on {other:?}"),
      ));
    }
  };

  if parent_path.is_empty() {
    Ok(updated_parent)
  } else {
    set_at_path(value, &parent_path, updated_parent)
  }
}

/// Build a minimal object from field name to value.
pub fn object(fields: impl IntoIterator<Item = (impl Into<String>, Unknown)>) -> Unknown {
  Unknown::Object(
    fields
      .into_iter()
      .map(|(k, v)| (k.into(), v))
      .collect::<BTreeMap<_, _>>(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::schema::Unknown;
  use rstest::rstest;

  fn sample() -> Unknown {
    object([
      ("name", Unknown::String("Ada".into())),
      (
        "tags",
        Unknown::Array(vec![
          Unknown::String("fp".into()),
          Unknown::String("rust".into()),
        ]),
      ),
    ])
  }

  mod get_at_path {
    use super::*;

    #[test]
    fn reads_top_level_field() {
      let value = sample();
      assert_eq!(
        get_at_path(&value, "name").unwrap(),
        &Unknown::String("Ada".into())
      );
    }

    #[test]
    fn reads_array_index() {
      let value = sample();
      assert_eq!(
        get_at_path(&value, "tags.1").unwrap(),
        &Unknown::String("rust".into())
      );
    }

    #[test]
    fn returns_error_for_missing_field() {
      let err = get_at_path(&sample(), "missing").unwrap_err();
      assert_eq!(err.path, "missing");
    }

    #[test]
    fn returns_error_for_non_object_traversal() {
      let err = get_at_path(&sample(), "name.oops").unwrap_err();
      assert!(err.message.contains("cannot traverse"));
    }
  }

  mod set_at_path {
    use super::*;

    #[test]
    fn replaces_nested_field() {
      let updated = set_at_path(sample(), "name", Unknown::String("Grace".into())).unwrap();
      assert_eq!(
        get_at_path(&updated, "name").unwrap(),
        &Unknown::String("Grace".into())
      );
    }

    #[test]
    fn replaces_array_element() {
      let updated = set_at_path(sample(), "tags.0", Unknown::String("logic".into())).unwrap();
      assert_eq!(
        get_at_path(&updated, "tags.0").unwrap(),
        &Unknown::String("logic".into())
      );
    }
  }

  #[rstest]
  #[case::empty_path("", Unknown::I64(1))]
  fn set_at_path_empty_replaces_root(#[case] path: &str, #[case] replacement: Unknown) {
    let root = Unknown::Null;
    let updated = set_at_path(root, path, replacement.clone()).unwrap();
    assert_eq!(updated, replacement);
  }
}
