//! Field-path access for [`id_effect::schema::Unknown`] values.

pub use crate::path::SchemaPathError;

use crate::path::{PathSegment, parse_path, segments_to_dot};
use id_effect::schema::Unknown;
use std::collections::BTreeMap;

/// Read a nested field from [`Unknown`] using dot-separated or JSON Pointer paths.
pub fn get_at_path<'a>(value: &'a Unknown, path: &str) -> Result<&'a Unknown, SchemaPathError> {
  if path.is_empty() {
    return Ok(value);
  }
  let segments = parse_path(path)?;
  get_at_segments(value, &segments, path)
}

fn get_at_segments<'a>(
  value: &'a Unknown,
  segments: &[PathSegment],
  path: &str,
) -> Result<&'a Unknown, SchemaPathError> {
  let mut current = value;
  for (idx, segment) in segments.iter().enumerate() {
    let partial = segments_to_dot(&segments[..=idx]);
    current = match (current, segment) {
      (Unknown::Object(map), PathSegment::Field(name)) => map
        .get(name)
        .ok_or_else(|| SchemaPathError::new(&partial, format!("missing field `{name}`")))?,
      (Unknown::Array(items), PathSegment::Index(index)) => items
        .get(*index)
        .ok_or_else(|| SchemaPathError::new(&partial, format!("index {index} out of bounds")))?,
      (Unknown::Array(_), PathSegment::Append) => {
        return Err(SchemaPathError::new(path, "append segment is not readable"));
      }
      (other, PathSegment::Field(name)) => {
        return Err(SchemaPathError::new(
          partial,
          format!("cannot traverse {other:?} for field `{name}`"),
        ));
      }
      (other, PathSegment::Index(index)) => {
        return Err(SchemaPathError::new(
          partial,
          format!("cannot traverse {other:?} at index {index}"),
        ));
      }
      (other, PathSegment::Append) => {
        return Err(SchemaPathError::new(
          partial,
          format!("cannot traverse {other:?} for append segment"),
        ));
      }
    };
  }
  Ok(current)
}

/// Set (replace) a nested field, cloning the tree along the path.
///
/// The target path and all parents must already exist.
pub fn set_at_path(
  value: Unknown,
  path: &str,
  new_value: Unknown,
) -> Result<Unknown, SchemaPathError> {
  if path.is_empty() {
    return Ok(new_value);
  }
  let segments = parse_path(path)?;
  set_at_segments(value, &segments, new_value, path)
}

/// Create or replace a nested field, materializing missing intermediate objects/arrays.
pub fn create_at_path(
  value: Unknown,
  path: &str,
  new_value: Unknown,
) -> Result<Unknown, SchemaPathError> {
  if path.is_empty() {
    return Ok(new_value);
  }
  let segments = parse_path(path)?;
  create_at_segments(value, &segments, new_value, path)
}

fn set_at_segments(
  value: Unknown,
  segments: &[PathSegment],
  new_value: Unknown,
  path: &str,
) -> Result<Unknown, SchemaPathError> {
  if segments.is_empty() {
    return Ok(new_value);
  }

  let parent_segments = &segments[..segments.len() - 1];
  let last = segments.last().expect("non-empty segments");

  let parent = if parent_segments.is_empty() {
    value.clone()
  } else {
    get_at_segments(&value, parent_segments, path)?.clone()
  };

  let updated_parent = assign_segment(parent, last, new_value, path, false)?;
  if parent_segments.is_empty() {
    Ok(updated_parent)
  } else {
    set_at_segments(value, parent_segments, updated_parent, path)
  }
}

fn create_at_segments(
  value: Unknown,
  segments: &[PathSegment],
  new_value: Unknown,
  path: &str,
) -> Result<Unknown, SchemaPathError> {
  if segments.is_empty() {
    return Ok(new_value);
  }

  let parent_segments = &segments[..segments.len() - 1];
  let last = segments.last().expect("non-empty segments");

  let parent = if parent_segments.is_empty() {
    value.clone()
  } else if get_at_segments(&value, parent_segments, path).is_ok() {
    get_at_segments(&value, parent_segments, path)?.clone()
  } else {
    let placeholder = default_container_for(last);
    let ensured = create_at_segments(value.clone(), parent_segments, placeholder, path)?;
    get_at_segments(&ensured, parent_segments, path)?.clone()
  };

  let updated_parent = assign_segment(parent, last, new_value, path, true)?;
  if parent_segments.is_empty() {
    Ok(updated_parent)
  } else {
    create_at_segments(value, parent_segments, updated_parent, path)
  }
}

fn default_container_for(segment: &PathSegment) -> Unknown {
  match segment {
    PathSegment::Field(_) => Unknown::Object(BTreeMap::new()),
    PathSegment::Index(_) | PathSegment::Append => Unknown::Array(Vec::new()),
  }
}

fn assign_segment(
  parent: Unknown,
  segment: &PathSegment,
  new_value: Unknown,
  path: &str,
  allow_append: bool,
) -> Result<Unknown, SchemaPathError> {
  match (parent, segment) {
    (Unknown::Object(mut map), PathSegment::Field(name)) => {
      map.insert(name.clone(), new_value);
      Ok(Unknown::Object(map))
    }
    (Unknown::Array(mut items), PathSegment::Index(index)) => {
      if *index > items.len() || (!allow_append && *index >= items.len()) {
        return Err(SchemaPathError::new(
          path.to_string(),
          format!("index {index} out of bounds"),
        ));
      }
      if *index == items.len() {
        items.push(new_value);
      } else {
        items[*index] = new_value;
      }
      Ok(Unknown::Array(items))
    }
    (Unknown::Array(mut items), PathSegment::Append) => {
      if !allow_append {
        return Err(SchemaPathError::new(
          path.to_string(),
          "append segment is not settable",
        ));
      }
      items.push(new_value);
      Ok(Unknown::Array(items))
    }
    (other, segment) => Err(SchemaPathError::new(
      path.to_string(),
      format!("cannot set {segment:?} on {other:?}"),
    )),
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
    fn reads_json_pointer_path() {
      let value = sample();
      assert_eq!(
        get_at_path(&value, "/tags/1").unwrap(),
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

    #[test]
    fn rejects_missing_intermediate_path() {
      let err = set_at_path(sample(), "new.field", Unknown::I64(3)).unwrap_err();
      assert_eq!(err.path, "new");
    }
  }

  mod create_at_path {
    use super::*;

    #[test]
    fn creates_nested_object_path() {
      let updated = create_at_path(sample(), "b.c", Unknown::I64(2)).unwrap();
      assert_eq!(get_at_path(&updated, "b.c").unwrap(), &Unknown::I64(2));
    }

    #[test]
    fn appends_array_element_with_dash() {
      let arr = Unknown::Array(vec![Unknown::I64(1)]);
      let updated = create_at_path(arr, "-", Unknown::I64(2)).unwrap();
      assert_eq!(
        updated,
        Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)])
      );
    }

    #[test]
    fn appends_nested_array_element() {
      let doc = object([("items", Unknown::Array(vec![Unknown::I64(1)]))]);
      let updated = create_at_path(doc, "items.-", Unknown::I64(2)).unwrap();
      assert_eq!(
        get_at_path(&updated, "items").unwrap(),
        &Unknown::Array(vec![Unknown::I64(1), Unknown::I64(2)])
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
