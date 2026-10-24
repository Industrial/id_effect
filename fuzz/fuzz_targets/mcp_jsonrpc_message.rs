//! MCP JSON-RPC messages (id_effect_ai `mcp::decode_message`, feature `mcp`, 0.13): the decoder
//! every MCP transport (stdio, Streamable HTTP, the client) reads a peer's bytes through. The
//! whole input is decoded under the default 1 MiB cap; the first byte also picks a small cap
//! (`× 16` bytes) for `decode_message_with_limit`, so the size refusal is reached.
//!
//! Checks besides "no panic": a message over the cap is refused before it is parsed; an accepted
//! message is a request, notification, response or error response with a `method` of 1 to
//! `MAX_NAME_BYTES` bytes, a string id of at most `MAX_NAME_BYTES`, `params` an object; its
//! re-encoding decodes to a message of the same kind, method, id and error; every refusal maps to
//! the JSON-RPC parse or invalid-request error.
#![no_main]

use id_effect_ai::mcp::{
  DEFAULT_MAX_MESSAGE_BYTES, DecodeError, INVALID_REQUEST, MAX_NAME_BYTES, Message, PARSE_ERROR,
  RequestId, decode_message, decode_message_with_limit,
};
use libfuzzer_sys::fuzz_target;

/// The parts of a message that survive any JSON re-encoding (floats inside `params` or `result`
/// may not, so they are left out).
fn shape(msg: &Message) -> (u8, Option<&str>, Option<&RequestId>, Option<(i64, &str)>) {
  match msg {
    Message::Request(r) => (0, Some(&r.method), Some(&r.id), None),
    Message::Notification(n) => (1, Some(&n.method), None, None),
    Message::Response(r) => (2, None, Some(&r.id), None),
    Message::Error(e) => (
      3,
      None,
      e.id.as_ref(),
      Some((e.error.code, e.error.message.as_str())),
    ),
  }
}

fn check(bytes: &[u8], max: usize) {
  match decode_message_with_limit(bytes, max) {
    Ok(msg) => {
      assert!(bytes.len() <= max, "a message over the cap was decoded");
      if let Some(method) = msg.method() {
        assert!(
          (1..=MAX_NAME_BYTES).contains(&method.len()),
          "a method of {} bytes",
          method.len()
        );
      }
      let (_, _, id, _) = shape(&msg);
      if let Some(RequestId::String(s)) = id {
        assert!(
          s.len() <= MAX_NAME_BYTES,
          "a string id of {} bytes",
          s.len()
        );
      }
      let params = match &msg {
        Message::Request(r) => r.params.as_ref(),
        Message::Notification(n) => n.params.as_ref(),
        _ => None,
      };
      assert!(
        params.is_none_or(serde_json::Value::is_object),
        "non-object params"
      );
      let text = msg.to_text();
      assert!(!text.contains('\n'), "a message is one line");
      // Uncapped: a re-encoding can be longer than its input (`1e2` is written `100.0`).
      let again = decode_message_with_limit(text.as_bytes(), usize::MAX)
        .expect("a message's own encoding decodes");
      assert_eq!(
        shape(&again),
        shape(&msg),
        "the message changed through a round trip"
      );
    }
    Err(err) => {
      if let DecodeError::TooLarge { max: cap } = err {
        assert_eq!(cap, max);
        assert!(bytes.len() > max);
      }
      let code = err.to_rpc_error().code;
      assert!(
        code == PARSE_ERROR || code == INVALID_REQUEST,
        "code {code}"
      );
    }
  }
}

fuzz_target!(|data: &[u8]| {
  check(data, DEFAULT_MAX_MESSAGE_BYTES);
  assert_eq!(
    decode_message(data).is_ok(),
    decode_message_with_limit(data, DEFAULT_MAX_MESSAGE_BYTES).is_ok()
  );
  if let [cap, body @ ..] = data {
    check(body, usize::from(*cap) * 16);
  }
});
