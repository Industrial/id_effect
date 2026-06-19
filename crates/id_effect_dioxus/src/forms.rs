//! Form decoding at the HTTP edge (urlencoded stub).

use serde::Deserialize;
use thiserror::Error;

/// Single decoded form field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormField {
  /// Field name.
  pub name: String,
  /// Raw value string.
  pub value: String,
}

/// Parsed form body ready for schema validation in `Effect`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormSubmission {
  /// Ordered fields as submitted.
  pub fields: Vec<FormField>,
}

/// Form decode failures.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FormError {
  /// Missing required field.
  #[error("missing field: {0}")]
  MissingField(String),
  /// Duplicate field name.
  #[error("duplicate field: {0}")]
  DuplicateField(String),
  /// Empty body.
  #[error("empty form body")]
  EmptyBody,
}

/// Decode `application/x-www-form-urlencoded` body (no percent-decoding edge cases in v1).
pub fn decode_form(body: &str) -> Result<FormSubmission, FormError> {
  if body.trim().is_empty() {
    return Err(FormError::EmptyBody);
  }
  let mut fields = Vec::new();
  let mut seen = std::collections::HashSet::new();
  for pair in body.split('&') {
    let (name, value) = pair
      .split_once('=')
      .map(|(k, v)| (k.to_string(), v.to_string()))
      .unwrap_or_else(|| (pair.to_string(), String::new()));
    if !seen.insert(name.clone()) {
      return Err(FormError::DuplicateField(name));
    }
    fields.push(FormField { name, value });
  }
  Ok(FormSubmission { fields })
}

/// Lookup a field by name.
#[inline]
pub fn require_field<'a>(form: &'a FormSubmission, name: &str) -> Result<&'a str, FormError> {
  form
    .fields
    .iter()
    .find(|f| f.name == name)
    .map(|f| f.value.as_str())
    .ok_or_else(|| FormError::MissingField(name.to_string()))
}
