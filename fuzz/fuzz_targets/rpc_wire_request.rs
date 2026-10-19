//! The JSON envelopes of id_effect_rpc (`wire`): the `WireCall` a client posts to `/rpc`
//! (`{"id"?, "tag", "v"?, "payload"}`, decoded with `CodecId::Json` as the server does), and the
//! `WireResult` / `WireExit` a client reads back. A tag that passes `is_valid_tag` is what the
//! router resolves.
//!
//! Checks besides "no panic": a decoded call re-encodes and decodes to the same id, tag and
//! version (the payload is any JSON, whose numbers may change spelling, so it is not compared);
//! a decoded exit keeps its success or failure through a round trip.
#![no_main]

use id_effect_rpc::codec::CodecId;
use id_effect_rpc::transport::frame::is_valid_tag;
use id_effect_rpc::wire::{WireCall, WireExit, WireResult};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
  let json = CodecId::Json;
  if let Ok(call) = json.decode::<WireCall>(data) {
    let _ = is_valid_tag(&call.tag);
    let again = json.encode(&call).expect("a decoded call encodes");
    let back: WireCall = json.decode(&again).expect("re-decodes");
    assert_eq!(
      (back.id, &back.tag, back.v),
      (call.id, &call.tag, call.v),
      "call envelope changed in a round trip"
    );
  }
  if let Ok(result) = json.decode::<WireResult>(data) {
    let again = json.encode(&result).expect("a decoded result encodes");
    let back: WireResult = json.decode(&again).expect("re-decodes");
    assert_eq!((back.id, &back.tag), (result.id, &result.tag));
    assert_eq!(back.exit.is_success(), result.exit.is_success());
  }
  if let Ok(exit) = json.decode::<WireExit>(data) {
    let again = json.encode(&exit).expect("a decoded exit encodes");
    let back: WireExit = json.decode(&again).expect("re-decodes");
    assert_eq!(back.is_success(), exit.is_success());
  }
});
