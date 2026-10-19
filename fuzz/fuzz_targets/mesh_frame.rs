//! Mesh wire frames (id_effect_node `wire::decode`, protocol v3): every frame a peer can send
//! after the transport connects, and the process envelope inside a data frame.
//!
//! Checks besides "no panic": a decoded frame re-encodes to bytes that decode to the same frame.
#![no_main]

use id_effect_node::wire::{Frame, decode, encode};
use id_effect_node::{decode_envelope, encode_envelope};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
  let Ok(frame) = decode(data) else {
    return;
  };
  let bytes = encode(&frame).expect("a decoded frame encodes");
  assert_eq!(
    decode(&bytes).expect("re-decodes"),
    frame,
    "frame changed in a round trip"
  );
  if let Frame::Data(payload) = &frame
    && let Ok(envelope) = decode_envelope(payload)
  {
    let again = encode_envelope(&envelope).expect("a decoded envelope encodes");
    assert_eq!(decode_envelope(&again).expect("re-decodes"), envelope);
  }
});
