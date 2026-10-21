//! Field values in and out of the admin: the kind of value a field holds (from its
//! [`FieldMeta::schema_type`](id_effect_sql::FieldMeta)), untrusted text parsed into a typed
//! [`SqlValue`] (filters, ids in paths), values shown as text, and URL encoding.

use id_effect_sql::SqlValue;

/// The longest text accepted for one typed value (a filter, an id), in bytes (ADR 0013 R4).
pub(crate) const MAX_VALUE_LEN: usize = 256;

/// What a field holds, from the last path segment of its schema type (`"i64"`,
/// `"jiff::Timestamp"` → `Timestamp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValueKind {
  Bool,
  I16,
  I32,
  I64,
  F32,
  F64,
  Text,
  Uuid,
  Date,
  Timestamp,
  /// Anything else (JSON, bytes, decimals, custom types): shown, never parsed or sorted.
  Other,
}

impl ValueKind {
  pub(crate) fn of(schema_type: &str) -> Self {
    let name = schema_type
      .rsplit("::")
      .next()
      .unwrap_or(schema_type)
      .trim();
    match name {
      "bool" => Self::Bool,
      "i8" | "i16" | "u8" => Self::I16,
      "i32" | "u16" => Self::I32,
      "i64" | "u32" | "isize" => Self::I64,
      "f32" => Self::F32,
      "f64" => Self::F64,
      "String" | "str" | "&str" | "&'static str" => Self::Text,
      "Uuid" => Self::Uuid,
      "Date" => Self::Date,
      "Timestamp" => Self::Timestamp,
      _ => Self::Other,
    }
  }

  /// Whether text from a request can be parsed into this kind.
  pub(crate) fn parses(self) -> bool {
    self != Self::Other
  }

  /// Whether a column of this kind can be a sort (and so a keyset cursor) key.
  pub(crate) fn sorts(self) -> bool {
    self != Self::Other
  }

  /// `text` (untrusted) as a value of this kind: `None` when it does not parse, is longer than
  /// [`MAX_VALUE_LEN`], or is a non-finite float.
  pub(crate) fn parse(self, text: &str) -> Option<SqlValue> {
    if text.len() > MAX_VALUE_LEN {
      return None;
    }
    let text = text.trim();
    Some(match self {
      Self::Bool => SqlValue::Bool(match text.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => true,
        "false" | "0" | "no" | "off" => false,
        _ => return None,
      }),
      Self::I16 => SqlValue::I16(text.parse().ok()?),
      Self::I32 => SqlValue::I32(text.parse().ok()?),
      Self::I64 => SqlValue::I64(text.parse().ok()?),
      Self::F32 => SqlValue::F32(text.parse::<f32>().ok().filter(|f| f.is_finite())?),
      Self::F64 => SqlValue::F64(text.parse::<f64>().ok().filter(|f| f.is_finite())?),
      Self::Text => SqlValue::Text(text.to_owned()),
      Self::Uuid => SqlValue::Uuid(uuid::Uuid::parse_str(text).ok()?),
      Self::Date => SqlValue::Date(text.parse().ok()?),
      Self::Timestamp => SqlValue::Timestamp(text.parse().ok()?),
      Self::Other => return None,
    })
  }
}

/// `value` as text: booleans through `yes` / `no`, NULL as the empty string.
pub(crate) fn display(value: &SqlValue, yes: &str, no: &str) -> String {
  match value {
    SqlValue::Null => String::new(),
    SqlValue::Bool(b) => if *b { yes } else { no }.to_owned(),
    SqlValue::I16(n) => n.to_string(),
    SqlValue::I32(n) => n.to_string(),
    SqlValue::I64(n) => n.to_string(),
    SqlValue::F32(n) => n.to_string(),
    SqlValue::F64(n) => n.to_string(),
    SqlValue::Text(s) => s.clone(),
    SqlValue::Bytes(b) => format!("{} bytes", b.len()),
    SqlValue::Uuid(u) => u.to_string(),
    SqlValue::Timestamp(t) => t.to_string(),
    SqlValue::DateTime(t) => t.to_string(),
    SqlValue::Date(d) => d.to_string(),
    SqlValue::Time(t) => t.to_string(),
    SqlValue::Decimal(d) => d.to_string(),
    SqlValue::Json(j) => j.to_string(),
    other => format!("{other:?}"),
  }
}

/// `value` (an id) as the text of a path segment, the inverse of [`ValueKind::parse`].
pub(crate) fn id_text(value: &SqlValue) -> String {
  display(value, "true", "false")
}

/// At most `max` characters of `text`, with `…` when cut.
pub(crate) fn truncate(text: String, max: usize) -> String {
  match text.char_indices().nth(max) {
    Some((at, _)) => {
      let mut cut = text[..at].to_owned();
      cut.push('…');
      cut
    }
    None => text,
  }
}

/// `text` percent-encoded for a URL path segment or query component: everything but the
/// unreserved characters (`A-Z a-z 0-9 - . _ ~`) as `%XX`.
pub(crate) fn encode(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  for b in text.bytes() {
    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
      out.push(char::from(b));
    } else {
      out.push_str(&format!("%{b:02X}"));
    }
  }
  out
}

/// `key=value` pairs as a query string (`?a=1&b=2`; empty without pairs).
pub(crate) fn query_string<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
  let parts: Vec<String> = pairs
    .into_iter()
    .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
    .collect();
  if parts.is_empty() {
    String::new()
  } else {
    format!("?{}", parts.join("&"))
  }
}

/// `field_name` as a label: `created_at` → `Created at`.
pub(crate) fn humanize(field_name: &str) -> String {
  let spaced = field_name.replace('_', " ");
  let mut chars = spaced.trim().chars();
  match chars.next() {
    Some(first) => first.to_uppercase().chain(chars).collect(),
    None => String::new(),
  }
}

/// `TypeName` as `type_name`.
pub(crate) fn snake_case(type_name: &str) -> String {
  let mut out = String::with_capacity(type_name.len() + 4);
  for (i, c) in type_name.chars().enumerate() {
    if c.is_uppercase() {
      if i > 0 {
        out.push('_');
      }
      out.extend(c.to_lowercase());
    } else {
      out.push(c);
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn kinds_from_schema_types() {
    let kinds: Vec<ValueKind> = [
      "bool",
      "i16",
      "i32",
      "i64",
      "u32",
      "f32",
      "f64",
      "String",
      "std::string::String",
      "uuid::Uuid",
      "jiff::civil::Date",
      "jiff::Timestamp",
      "serde_json::Value",
    ]
    .iter()
    .map(|t| ValueKind::of(t))
    .collect();
    use ValueKind::*;
    assert_eq!(
      kinds,
      [
        Bool, I16, I32, I64, I64, F32, F64, Text, Text, Uuid, Date, Timestamp, Other
      ]
    );
    assert!(!Other.parses() && !Other.sorts() && Text.parses() && Date.sorts());
  }

  #[test]
  fn parse_is_typed_and_bounded() {
    use ValueKind::*;
    assert_eq!(Bool.parse(" Yes "), Some(SqlValue::Bool(true)));
    assert_eq!(Bool.parse("off"), Some(SqlValue::Bool(false)));
    assert_eq!(Bool.parse("maybe"), None);
    assert_eq!(I16.parse("7"), Some(SqlValue::I16(7)));
    assert_eq!(I32.parse("-7"), Some(SqlValue::I32(-7)));
    assert_eq!(I64.parse("x"), None);
    assert_eq!(F32.parse("1.5"), Some(SqlValue::F32(1.5)));
    assert_eq!(F64.parse("NaN"), None);
    assert_eq!(F64.parse("2.5"), Some(SqlValue::F64(2.5)));
    assert_eq!(Text.parse("a b"), Some(SqlValue::Text("a b".into())));
    assert_eq!(Text.parse(&"x".repeat(MAX_VALUE_LEN + 1)), None);
    let id = "67e55044-10b1-426f-9247-bb680e5fe0c8";
    assert_eq!(Uuid.parse(id).map(|v| id_text(&v)), Some(id.to_owned()));
    assert_eq!(Uuid.parse("nope"), None);
    assert!(matches!(Date.parse("2026-10-21"), Some(SqlValue::Date(_))));
    assert!(matches!(
      Timestamp.parse("2026-10-21T10:00:00Z"),
      Some(SqlValue::Timestamp(_))
    ));
    assert_eq!(Other.parse("{}"), None);
  }

  #[test]
  fn display_covers_every_value() {
    let ts: jiff::Timestamp = "2026-10-21T10:00:00Z".parse().unwrap();
    let dt = jiff::civil::date(2026, 10, 21).at(10, 0, 0, 0);
    let shown: Vec<String> = [
      SqlValue::Null,
      SqlValue::Bool(true),
      SqlValue::Bool(false),
      SqlValue::I16(1),
      SqlValue::I32(2),
      SqlValue::I64(3),
      SqlValue::F32(0.5),
      SqlValue::F64(1.5),
      SqlValue::Text("t".into()),
      SqlValue::Bytes(vec![1, 2]),
      SqlValue::Uuid(uuid::Uuid::nil()),
      SqlValue::Timestamp(ts),
      SqlValue::DateTime(dt),
      SqlValue::Date(dt.date()),
      SqlValue::Time(dt.time()),
      SqlValue::Decimal(Default::default()),
      SqlValue::Json(serde_json::Value::Null),
    ]
    .iter()
    .map(|v| display(v, "yes", "no"))
    .collect();
    assert_eq!(
      shown[..10],
      ["", "yes", "no", "1", "2", "3", "0.5", "1.5", "t", "2 bytes"]
    );
    assert_eq!(shown[10], "00000000-0000-0000-0000-000000000000");
    assert_eq!(shown[11], "2026-10-21T10:00:00Z");
    assert_eq!(shown[12], "2026-10-21T10:00:00");
    assert_eq!(shown[13], "2026-10-21");
    assert_eq!(shown[14], "10:00:00");
    assert_eq!(shown[15], "0");
    assert_eq!(shown[16], "null");
  }

  #[test]
  fn text_helpers() {
    assert_eq!(truncate("abcdef".into(), 3), "abc…");
    assert_eq!(truncate("abc".into(), 3), "abc");
    assert_eq!(encode("a b/ä~"), "a%20b%2F%C3%A4~");
    assert_eq!(
      query_string([("q", "x y"), ("f.status", "a&b")]),
      "?q=x%20y&f.status=a%26b"
    );
    assert_eq!(query_string([]), "");
    assert_eq!(humanize("created_at"), "Created at");
    assert_eq!(humanize(""), "");
    assert_eq!(snake_case("DocumentLabel"), "document_label");
    assert_eq!(snake_case("document"), "document");
  }
}
