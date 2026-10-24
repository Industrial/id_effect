//! Keyset page cursors (id_effect_sql `repository::PageCursor::decode`, feature `repository`,
//! 0.13 decision D11): the opaque token a client sends back to read the next or previous page.
//! The fuzzed bytes are tried as the token itself and, base64url-encoded, as the cursor document
//! inside it (so the JSON and the cursor `Schema` see fuzzed input past the base64 step).
//!
//! Checks besides "no panic": a token over `MAX_CURSOR_BYTES` is refused before it is decoded; an
//! accepted cursor has 1 to `MAX_SORT_KEYS` keys, none NULL or JSON, and round-trips: its encoding
//! (when within the cap) decodes to the same cursor and is accepted by `PageCursor::new`.
#![no_main]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use id_effect_sql::SqlValue;
use id_effect_sql::repository::page::{
  MAX_CURSOR_BYTES, MAX_SORT_KEYS, PageCursor, PageCursorError,
};
use libfuzzer_sys::fuzz_target;

fn check(token: &str) {
  match PageCursor::decode(token) {
    Ok(cursor) => {
      assert!(
        token.len() <= MAX_CURSOR_BYTES,
        "a token over the cap was decoded"
      );
      let keys = cursor.keys();
      assert!(
        (1..=MAX_SORT_KEYS).contains(&keys.len()),
        "{} keys",
        keys.len()
      );
      assert!(
        !keys
          .iter()
          .any(|k| matches!(k, SqlValue::Null | SqlValue::Json(_))),
        "a NULL or JSON key"
      );
      let encoded = cursor.encode();
      if encoded.len() <= MAX_CURSOR_BYTES {
        let again = PageCursor::decode(&encoded).expect("a cursor's own encoding decodes");
        assert_eq!(again, cursor, "the cursor changed through a round trip");
        let rebuilt = PageCursor::new(cursor.fingerprint(), keys.to_vec())
          .expect("a decoded cursor is a valid cursor");
        assert_eq!(rebuilt, cursor);
      }
    }
    Err(PageCursorError::TooLong { len }) => {
      assert_eq!(len, token.len());
      assert!(len > MAX_CURSOR_BYTES);
    }
    Err(_) => {}
  }
}

fuzz_target!(|data: &[u8]| {
  if let Ok(token) = std::str::from_utf8(data) {
    check(token);
  }
  check(&URL_SAFE_NO_PAD.encode(data));
});
