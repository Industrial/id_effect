//! The Streamable HTTP routing headers (`Mcp-Method`, `Mcp-Name`, `Mcp-Param-*`): built by the
//! client from a request, checked against the body by the server (ADR 0015 §7). A value that
//! cannot travel as a plain header value (non-ASCII or control characters, surrounding spaces)
//! is sent as `=?base64?<standard base64>?=`. Names and values are validated, so a request can
//! never smuggle a header (B23).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::Value;

use super::message::Message;

/// `MCP-Protocol-Version`: the revision the request speaks.
pub const HEADER_PROTOCOL_VERSION: &str = "mcp-protocol-version";
/// `Mcp-Method`: the JSON-RPC method of the body.
pub const HEADER_METHOD: &str = "mcp-method";
/// `Mcp-Name`: the tool or prompt name, or the resource URI, of the body.
pub const HEADER_NAME: &str = "mcp-name";
/// Prefix of the `Mcp-Param-<name>` headers mirroring `x-mcp-header` tool parameters.
pub const HEADER_PARAM_PREFIX: &str = "mcp-param-";

const B64_PREFIX: &str = "=?base64?";
const B64_SUFFIX: &str = "?=";

/// Longest header value sent or accepted (bytes, before encoding).
const MAX_HEADER_VALUE: usize = 4096;

/// The `params` member whose value goes in `Mcp-Name`, per method.
fn name_key(method: &str) -> Option<&'static str> {
  match method {
    "tools/call" | "prompts/get" => Some("name"),
    "resources/read" | "resources/subscribe" | "resources/unsubscribe" => Some("uri"),
    _ => None,
  }
}

/// The value a request's `Mcp-Name` must carry, if its method has one.
pub(crate) fn expected_name(method: &str, params: Option<&Value>) -> Option<String> {
  let key = name_key(method)?;
  params?.get(key)?.as_str().map(str::to_owned)
}

/// Encodes `value` for a header.
pub(crate) fn encode_value(value: &str) -> String {
  let plain = !value.starts_with([' ', '\t'])
    && !value.ends_with([' ', '\t'])
    && value.bytes().all(|b| (0x20..0x7f).contains(&b))
    && !(value.starts_with(B64_PREFIX) && value.ends_with(B64_SUFFIX));
  if plain {
    value.to_owned()
  } else {
    format!("{B64_PREFIX}{}{B64_SUFFIX}", BASE64.encode(value))
  }
}

/// Decodes a header value written by [`encode_value`] (`None` for an invalid base64 form).
pub(crate) fn decode_value(raw: &str) -> Option<String> {
  match raw
    .strip_prefix(B64_PREFIX)
    .and_then(|r| r.strip_suffix(B64_SUFFIX))
  {
    Some(inner) => String::from_utf8(BASE64.decode(inner).ok()?).ok(),
    None => Some(raw.to_owned()),
  }
}

/// Whether `name` is an RFC 9110 token (a valid header name part).
fn is_token(name: &str) -> bool {
  !name.is_empty()
    && name.bytes().all(|b| {
      b.is_ascii_alphanumeric()
        || matches!(
          b,
          b'!'
            | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
        )
    })
}

/// The `(argument, header suffix)` pairs a tool schema marks with `x-mcp-header` (top-level
/// primitive properties with a valid token only; anything else is ignored).
fn param_annotations(schema: &Value) -> Vec<(String, String)> {
  let Some(Value::Object(props)) = schema.get("properties") else {
    return Vec::new();
  };
  props
    .iter()
    .filter_map(|(prop, s)| {
      let header = s.get("x-mcp-header")?.as_str()?;
      let primitive = matches!(
        s.get("type").and_then(Value::as_str),
        Some("string" | "integer" | "boolean")
      );
      (primitive && is_token(header)).then(|| (prop.clone(), header.to_ascii_lowercase()))
    })
    .collect()
}

fn primitive_text(v: &Value) -> Option<String> {
  match v {
    Value::String(s) => Some(s.clone()),
    Value::Bool(b) => Some(b.to_string()),
    Value::Number(n) => Some(n.to_string()),
    _ => None,
  }
}

/// The routing headers of `msg` (lower-case names): `Mcp-Method`, `Mcp-Name` and, for
/// `tools/call` with the tool's input `schema`, the `Mcp-Param-*` mirrors.
pub(crate) fn request_headers(msg: &Message, schema: Option<&Value>) -> Vec<(String, String)> {
  let (method, params) = match msg {
    Message::Request(r) => (r.method.as_str(), r.params.as_ref()),
    Message::Notification(n) => (n.method.as_str(), n.params.as_ref()),
    Message::Response(_) | Message::Error(_) => return Vec::new(),
  };
  let mut out = vec![(HEADER_METHOD.to_owned(), method.to_owned())];
  if let Some(name) = expected_name(method, params).filter(|n| n.len() <= MAX_HEADER_VALUE) {
    out.push((HEADER_NAME.to_owned(), encode_value(&name)));
  }
  if method == "tools/call"
    && let (Some(schema), Some(args)) = (schema, params.and_then(|p| p.get("arguments")))
  {
    for (prop, header) in param_annotations(schema) {
      if let Some(text) = args
        .get(&prop)
        .and_then(primitive_text)
        .filter(|t| t.len() <= MAX_HEADER_VALUE)
      {
        out.push((
          format!("{HEADER_PARAM_PREFIX}{header}"),
          encode_value(&text),
        ));
      }
    }
  }
  out
}

/// Checks the routing headers (read through `header`, by lower-case name) against `msg`; the
/// reason when they disagree (answered `400` with [`HEADER_MISMATCH`](super::HEADER_MISMATCH)).
pub(crate) fn check_request_headers<'a>(
  header: impl Fn(&str) -> Option<&'a str>,
  msg: &Message,
  schema: Option<&Value>,
) -> Result<(), String> {
  let (method, params) = match msg {
    Message::Request(r) => (r.method.as_str(), r.params.as_ref()),
    Message::Notification(n) => (n.method.as_str(), n.params.as_ref()),
    Message::Response(_) | Message::Error(_) => return Ok(()),
  };
  match header(HEADER_METHOD) {
    None => return Err("missing Mcp-Method header".into()),
    Some(h) if h != method => return Err("Mcp-Method header does not match the body".into()),
    Some(_) => {}
  }
  if let Some(expected) = expected_name(method, params) {
    match header(HEADER_NAME).map(decode_value) {
      None => return Err("missing Mcp-Name header".into()),
      Some(Some(v)) if v == expected => {}
      Some(_) => return Err("Mcp-Name header does not match the body".into()),
    }
  }
  if method == "tools/call"
    && let Some(schema) = schema
  {
    let args = params.and_then(|p| p.get("arguments"));
    for (prop, header_name) in param_annotations(schema) {
      let full = format!("{HEADER_PARAM_PREFIX}{header_name}");
      let body = args
        .and_then(|a| a.get(&prop))
        .filter(|v| !v.is_null())
        .and_then(primitive_text);
      match (header(&full).map(decode_value), body) {
        (None, None) => {}
        (Some(Some(h)), Some(b)) if h == b => {}
        _ => return Err(format!("{full} header does not match the body")),
      }
    }
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::mcp::message::RequestId;
  use serde_json::json;
  use std::collections::HashMap;

  fn call(args: Value) -> Message {
    Message::request(
      RequestId::Number(1),
      "tools/call",
      Some(json!({ "name": "get", "arguments": args })),
    )
  }

  fn schema() -> Value {
    json!({ "type": "object", "properties": {
      "region": { "type": "string", "x-mcp-header": "Region" },
      "n": { "type": "integer", "x-mcp-header": "Count" },
      "nested": { "type": "object", "x-mcp-header": "Nested" },
      "bad": { "type": "string", "x-mcp-header": "bad header" },
      "plain": { "type": "string" },
    }})
  }

  fn check(
    headers: &[(String, String)],
    msg: &Message,
    schema: Option<&Value>,
  ) -> Result<(), String> {
    let map: HashMap<&str, &str> = headers
      .iter()
      .map(|(k, v)| (k.as_str(), v.as_str()))
      .collect();
    check_request_headers(|k| map.get(k).copied(), msg, schema)
  }

  #[test]
  fn values_encode_and_decode() {
    for v in [
      "plain",
      " lead",
      "trail\t",
      "naïve",
      "line\nbreak",
      "=?base64?x?=",
    ] {
      assert_eq!(decode_value(&encode_value(v)).as_deref(), Some(v));
    }
    assert_eq!(encode_value("plain"), "plain");
    assert!(encode_value("naïve").starts_with(B64_PREFIX));
    assert_eq!(decode_value("=?base64?***?="), None);
    assert!(is_token("X-Region_1"));
    assert!(!is_token(""));
    assert!(!is_token("a b"));
  }

  #[test]
  fn the_client_headers_pass_the_server_check() {
    let msg = call(json!({ "region": "eu-wést", "n": 3, "plain": "x" }));
    let headers = request_headers(&msg, Some(&schema()));
    let mut names: Vec<_> = headers.iter().map(|(k, _)| k.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
      names,
      [
        "mcp-method",
        "mcp-name",
        "mcp-param-count",
        "mcp-param-region"
      ]
    );
    assert_eq!(check(&headers, &msg, Some(&schema())), Ok(()));

    let read = Message::request(
      RequestId::Number(2),
      "resources/read",
      Some(json!({ "uri": "doc://1" })),
    );
    let headers = request_headers(&read, None);
    assert_eq!(headers[1], ("mcp-name".into(), "doc://1".into()));
    assert_eq!(check(&headers, &read, None), Ok(()));
    let note = Message::notification("notifications/cancelled", None);
    assert_eq!(check(&request_headers(&note, None), &note, None), Ok(()));
    let resp = Message::response(RequestId::Number(1), json!({}));
    assert!(request_headers(&resp, None).is_empty());
    assert_eq!(check(&[], &resp, None), Ok(()));
  }

  #[test]
  fn mismatches_are_reported() {
    let msg = call(json!({ "region": "eu" }));
    let s = schema();
    let good = request_headers(&msg, Some(&s));
    assert!(
      check(&[], &msg, None)
        .unwrap_err()
        .contains("missing Mcp-Method")
    );
    let mut wrong_method = good.clone();
    wrong_method[0].1 = "tools/list".into();
    assert!(check(&wrong_method, &msg, None).is_err());
    let no_name: Vec<_> = good
      .iter()
      .filter(|(k, _)| k != "mcp-name")
      .cloned()
      .collect();
    assert!(
      check(&no_name, &msg, None)
        .unwrap_err()
        .contains("missing Mcp-Name")
    );
    let mut other_name = good.clone();
    other_name[1].1 = "rm".into();
    assert!(check(&other_name, &msg, None).is_err());
    // A param header the body does not carry, or one that is missing, is refused.
    let mut extra = good.clone();
    extra.push(("mcp-param-count".into(), "9".into()));
    assert!(check(&extra, &msg, Some(&s)).is_err());
    let missing: Vec<_> = good
      .iter()
      .filter(|(k, _)| !k.starts_with(HEADER_PARAM_PREFIX))
      .cloned()
      .collect();
    assert!(check(&missing, &msg, Some(&s)).is_err());
    assert!(param_annotations(&json!({})).is_empty());
  }
}
