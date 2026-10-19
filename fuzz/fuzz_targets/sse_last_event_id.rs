//! The SSE `Last-Event-ID` header (id_effect_axum realtime, feature `sse`:
//! `LastEventId::parse`), as a reconnecting `EventSource` sends it, then what the RPC SSE
//! transport does with an accepted id: decode it as a `wire::Cursor` (the subscription resumes
//! after it; anything else is a 400).
//!
//! Checks besides "no panic": an accepted id is at most `MAX_LAST_EVENT_ID_BYTES` of UTF-8
//! without CR, LF or NUL, and is the input unchanged; a cursor decoded from it re-spells to it.
#![no_main]

use id_effect_axum::realtime::sse::{LastEventId, MAX_LAST_EVENT_ID_BYTES};
use id_effect_rpc::wire::Cursor;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
  let Ok(id) = LastEventId::parse(data) else {
    return;
  };
  assert!(data.len() <= MAX_LAST_EVENT_ID_BYTES, "an id over the cap");
  assert!(
    !data.iter().any(|b| matches!(b, b'\r' | b'\n' | 0)),
    "an id with CR, LF or NUL"
  );
  assert_eq!(id.as_str().as_bytes(), data, "the id changed");
  let _ = id.as_u64();
  if let Ok(cursor) = Cursor::decode(id.as_str()) {
    assert_eq!(cursor.to_string(), id.as_str(), "a cursor has one spelling");
  }
});
