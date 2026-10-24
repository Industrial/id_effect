//! Admin form bodies (id_effect_admin `forms::decode_form`, then the mass-assignment guard
//! `FormBody::bind`, 0.13): the `application/x-www-form-urlencoded` body of every admin create,
//! edit and bulk `POST`.
//!
//! Checks besides "no panic": a body over `MAX_FORM_BYTES` is refused as too large (413) and one
//! decoded has at most `MAX_FORM_FIELDS` pairs; the pairs re-encode (percent-escaping every byte
//! but unreserved ones) to a body that decodes to the same pairs; `bind` keeps exactly the pairs
//! of the declared fields, in order, and reports every other name once, never the CSRF field nor
//! an `_`-prefixed name.
#![no_main]

use std::fmt::Write as _;

use axum::http::StatusCode;
use id_effect_admin::forms::{FormError, MAX_FORM_BYTES, MAX_FORM_FIELDS, decode_form};
use id_effect_axum::server::security::CSRF_FORM_FIELD;
use libfuzzer_sys::fuzz_target;

/// The fields a form declares (an entity's editable fields).
const DECLARED: &[&str] = &["title", "body", "price"];

fn encode(text: &str, out: &mut String) {
  for b in text.bytes() {
    if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
      out.push(char::from(b));
    } else if b == b' ' {
      out.push('+');
    } else {
      let _ = write!(out, "%{b:02X}");
    }
  }
}

fuzz_target!(|data: &[u8]| {
  let body = match decode_form(data) {
    Ok(body) => body,
    Err(err) => {
      match err {
        FormError::TooLarge => assert!(data.len() > MAX_FORM_BYTES),
        _ => assert!(
          data.len() <= MAX_FORM_BYTES,
          "a body over the cap was parsed"
        ),
      }
      let status = err.status();
      assert_eq!(
        status == StatusCode::PAYLOAD_TOO_LARGE,
        err == FormError::TooLarge
      );
      assert!(status.is_client_error());
      return;
    }
  };
  assert!(data.len() <= MAX_FORM_BYTES);
  let pairs = body.pairs();
  assert!(pairs.len() <= MAX_FORM_FIELDS, "{} pairs", pairs.len());

  let mut again = String::new();
  for (i, (name, value)) in pairs.iter().enumerate() {
    if i > 0 {
      again.push('&');
    }
    encode(name, &mut again);
    again.push('=');
    encode(value, &mut again);
  }
  if again.len() <= MAX_FORM_BYTES {
    let reparsed = decode_form(again.as_bytes()).expect("a re-encoded body decodes");
    assert_eq!(reparsed, body, "the pairs changed through a round trip");
  }
  for (name, _) in pairs {
    assert_eq!(body.value(name), body.values(name).last());
  }

  let bound = body.bind(DECLARED);
  let declared: Vec<_> = pairs
    .iter()
    .filter(|(n, _)| DECLARED.contains(&n.as_str()))
    .cloned()
    .collect();
  assert_eq!(
    bound.values(),
    declared,
    "bind kept other pairs or lost declared ones"
  );
  for (i, name) in bound.ignored().iter().enumerate() {
    assert!(
      !DECLARED.contains(&name.as_str()),
      "a declared field reported ignored"
    );
    assert!(!name.starts_with('_'), "an admin field reported ignored");
    assert!(name != CSRF_FORM_FIELD, "the CSRF field reported ignored");
    assert!(
      !bound.ignored()[..i].contains(name),
      "{name} reported twice"
    );
    assert!(
      pairs.iter().any(|(n, _)| n == name),
      "{name} was not submitted"
    );
  }
});
