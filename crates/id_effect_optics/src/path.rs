//! Dot-separated and JSON Pointer path parsing for schema navigation.

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
  pub(crate) fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
    Self {
      path: path.into(),
      message: message.into(),
    }
  }
}

/// A single step in a document path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSegment {
  /// Object field name.
  Field(String),
  /// Array index.
  Index(usize),
  /// Append to an array (`-` in RFC 6902).
  Append,
}

/// Parse a path string into segments.
///
/// Accepts dot-separated paths (`user.name`, `tags.0`, `items.-`) and JSON
/// Pointer paths (`/user/name/0/-`). An empty path yields no segments.
pub fn parse_path(path: &str) -> Result<Vec<PathSegment>, SchemaPathError> {
  if path.is_empty() {
    return Ok(Vec::new());
  }

  if path.starts_with('/') {
    parse_json_pointer(path)
  } else {
    parse_dot_path(path)
  }
}

fn parse_dot_path(path: &str) -> Result<Vec<PathSegment>, SchemaPathError> {
  path.split('.').map(parse_segment).collect()
}

fn parse_json_pointer(path: &str) -> Result<Vec<PathSegment>, SchemaPathError> {
  if path == "/" {
    return Ok(Vec::new());
  }
  path
    .trim_start_matches('/')
    .split('/')
    .map(|segment| parse_segment(&decode_json_pointer_segment(segment)))
    .collect()
}

fn decode_json_pointer_segment(segment: &str) -> String {
  segment.replace("~1", "/").replace("~0", "~")
}

fn parse_segment(segment: &str) -> Result<PathSegment, SchemaPathError> {
  if segment == "-" {
    return Ok(PathSegment::Append);
  }
  if let Ok(index) = segment.parse::<usize>() {
    return Ok(PathSegment::Index(index));
  }
  Ok(PathSegment::Field(segment.to_string()))
}

/// Render segments as a dot-separated path (for error messages).
pub fn segments_to_dot(segments: &[PathSegment]) -> String {
  segments
    .iter()
    .map(|segment| match segment {
      PathSegment::Field(name) => name.clone(),
      PathSegment::Index(index) => index.to_string(),
      PathSegment::Append => "-".to_string(),
    })
    .collect::<Vec<_>>()
    .join(".")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parses_dot_segments() {
    let segments = parse_path("user.name").unwrap();
    assert_eq!(
      segments,
      vec![
        PathSegment::Field("user".into()),
        PathSegment::Field("name".into()),
      ]
    );
  }

  #[test]
  fn parses_array_append() {
    let segments = parse_path("items.-").unwrap();
    assert_eq!(
      segments,
      vec![PathSegment::Field("items".into()), PathSegment::Append]
    );
  }

  #[test]
  fn parses_json_pointer() {
    let segments = parse_path("/tags/0").unwrap();
    assert_eq!(
      segments,
      vec![PathSegment::Field("tags".into()), PathSegment::Index(0)]
    );
  }
}
