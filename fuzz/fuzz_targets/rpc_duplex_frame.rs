//! Frames of the multiplexed RPC transports (id_effect_rpc `transport::frame::decode`, shared by
//! WebSocket and WebTransport): the first input byte picks the codec, the rest is one message.
//!
//! - even: the `idrpc.json.v1` path, one WebSocket text message: `frame::decode(_, Json)`; a
//!   `resume` frame's payload then goes through `resume::decode_resume`, as the server does.
//! - odd: the `idrpc.postcard.v1` path, one binary message: `frame::decode(_, Postcard)` (refused:
//!   frames carry typed payloads), then the path the server takes, `codec::postcard::decode_frame`
//!   and `frame::validate`, and a `resume` payload as `wire::Resume` through `decode_resume`.
//!
//! Checks besides "no panic": a decoded frame passes `validate`, and re-encodes (when within the
//! cap: JSON numbers may grow) to a frame with the same envelope.
#![no_main]

use id_effect_rpc::codec::postcard::{Postcard, decode_frame};
use id_effect_rpc::codec::{Codec as _, CodecId};
use id_effect_rpc::resume::decode_resume;
use id_effect_rpc::transport::frame::{self, FrameError};
use id_effect_rpc::wire::{Frame, FrameKind, Resume};
use libfuzzer_sys::fuzz_target;

/// Everything but the payload.
fn envelope<P>(f: &Frame<P>) -> Frame<()> {
  Frame {
    id: f.id,
    kind: f.kind,
    flags: f.flags,
    tag: f.tag.clone(),
    v: f.v,
    cursor: f.cursor,
    credits: f.credits,
    payload: None,
  }
}

fn json(bytes: &[u8]) {
  let Ok(decoded) = frame::decode(bytes, CodecId::Json) else {
    return;
  };
  frame::validate(&decoded).expect("a decoded frame is valid");
  if decoded.kind == FrameKind::Resume
    && let Some(payload) = &decoded.payload
  {
    let _ = decode_resume(payload);
  }
  match frame::encode(&decoded, CodecId::Json) {
    Ok(again) => {
      let back = frame::decode(&again, CodecId::Json).expect("re-decodes");
      assert_eq!(
        envelope(&back),
        envelope(&decoded),
        "frame changed in a round trip"
      );
    }
    Err(FrameError::TooLarge { .. }) => {}
    Err(err) => panic!("a decoded frame does not encode: {err}"),
  }
}

fn postcard(bytes: &[u8]) {
  let _ = frame::decode(bytes, CodecId::Postcard);
  let Ok(raw) = decode_frame(bytes) else {
    return;
  };
  if frame::validate(&raw).is_err() {
    return;
  }
  if raw.kind == FrameKind::Resume
    && let Some(payload) = &raw.payload
    && let Ok(resume) = Postcard::decode::<Resume>(payload)
  {
    let value = serde_json::to_value(&resume).expect("a resume encodes as JSON");
    let _ = decode_resume(&value);
  }
}

fuzz_target!(|data: &[u8]| {
  let Some((&selector, message)) = data.split_first() else {
    return;
  };
  if selector % 2 == 0 {
    json(message);
  } else {
    postcard(message);
  }
});
