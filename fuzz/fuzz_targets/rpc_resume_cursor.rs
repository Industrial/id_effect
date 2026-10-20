//! Resume decoders of id_effect_rpc (`resume`): `decode_cursor` on the input as text (a cursor a
//! reconnecting client sends), and `decode_resume` on the input as a JSON `resume` payload
//! (`{"session", "calls": [{"id", "cursor"}]}`). The whole frame path, `transport::frame::decode`
//! then `decode_resume`, is in `rpc_duplex_frame`.
//!
//! Checks besides "no panic": a decoded cursor is a sequence cursor whose text is exactly the
//! input (one spelling per cursor: no padding, nothing trailing, at most 64 bytes); an accepted
//! resume has at most 1,024 distinct call ids, none 0, each with a sequence cursor.
#![no_main]

use std::collections::HashSet;

use id_effect_rpc::resume::{MAX_RESUME_CALLS, decode_cursor, decode_resume};
use id_effect_rpc::wire::{CallId, CursorSource, MAX_CURSOR_LEN};
use libfuzzer_sys::fuzz_target;
use serde_json::Value;

fuzz_target!(|data: &[u8]| {
  if let Ok(text) = std::str::from_utf8(data)
    && let Ok(cursor) = decode_cursor(text)
  {
    assert!(text.len() <= MAX_CURSOR_LEN, "a cursor over the length cap");
    assert_eq!(cursor.source, CursorSource::Sequence);
    assert_eq!(cursor.to_string(), text, "a cursor has one spelling");
  }
  let Ok(payload) = serde_json::from_slice::<Value>(data) else {
    return;
  };
  if let Ok(resume) = decode_resume(&payload) {
    assert!(resume.calls.len() <= MAX_RESUME_CALLS);
    let mut ids = HashSet::new();
    for call in &resume.calls {
      assert_ne!(call.id, CallId::CONNECTION, "call id 0 resumed");
      assert!(ids.insert(call.id), "call id resumed twice");
      assert_eq!(call.cursor.source, CursorSource::Sequence);
    }
  }
});
