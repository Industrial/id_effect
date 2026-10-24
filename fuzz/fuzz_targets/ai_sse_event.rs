//! Server-sent events from an AI vendor stream (id_effect_ai `stream::decode_sse_event` and the
//! incremental `SseParser` the vendor adapters read their streams with, 0.13). A vendor (or
//! whoever sits between it and the service) controls these bytes.
//!
//! Checks besides "no panic": input over `MAX_SSE_EVENT_BYTES` is refused; an accepted event has
//! a field (an `event:` or a `data:` line) whose text holds no line break; and the incremental
//! parser fed the same bytes in two chunks, split at a fuzzed point (inside a line or a UTF-8
//! character too), yields the same first event, never counting more pending bytes than it was
//! fed.
#![no_main]

use id_effect_ai::stream::{MAX_SSE_EVENT_BYTES, SseParser, decode_sse_event};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
  let [split, body @ ..] = data else {
    return;
  };
  let decoded = match decode_sse_event(body) {
    Ok(decoded) => decoded,
    Err(_) => {
      assert!(body.len() > MAX_SSE_EVENT_BYTES, "a refusal within the cap");
      return;
    }
  };
  if let Some(msg) = &decoded {
    assert!(
      msg.event.is_some() || !msg.data_lines.is_empty(),
      "an event without a field"
    );
    let no_break = |s: &str| !s.contains(['\n']);
    assert!(msg.event.as_deref().is_none_or(no_break));
    assert!(msg.data_lines.iter().all(|line| no_break(line)));
    assert_eq!(msg.data(), msg.data_lines.join("\n"));
  }

  let at = usize::from(*split) % (body.len() + 1);
  let mut parser = SseParser::new();
  // Pending fields are counted as decoded text: an invalid byte becomes U+FFFD (3 bytes).
  let mut events = parser.feed_bytes(&body[..at]);
  assert!(
    parser.pending_bytes() <= 3 * at,
    "pending bytes out of proportion"
  );
  events.extend(parser.feed_bytes(&body[at..]));
  assert!(
    parser.pending_bytes() <= 3 * body.len(),
    "pending bytes out of proportion"
  );
  let first = match events.into_iter().next() {
    Some(first) => Some(first),
    None => parser.finish(),
  };
  assert_eq!(first, decoded, "chunking changed the first event");
});
