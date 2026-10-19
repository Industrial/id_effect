//! The postcard codec of id_effect_rpc (feature `postcard`; `codec::postcard`): every decoder a
//! peer reaches with postcard bytes. The first input byte picks the decoder, the rest is its
//! input:
//!
//! | byte % 7 | decoder | reached by |
//! |---|---|---|
//! | 0 | `decode_frame` (+ `transport::frame::validate`) | `idrpc.postcard.v1` WebSocket messages, WebTransport records |
//! | 1 | `decode_call` | `POST /rpc` with the postcard media type |
//! | 2 | `decode_result` | the client reading a postcard answer |
//! | 3 | `decode_exit` | the client reading an `exit` payload |
//! | 4 | `split_record`, then `decode_frame` per record | postcard stream bodies, QUIC / WebTransport streams |
//! | 5 | `decode_limited::<Tree>` | a recursive procedure type (nesting bound) |
//! | 6 | `decode_limited::<Vec<()>>` | a long sequence of empty elements (element budget) |
//!
//! Checks besides "no panic": a decoded frame or call re-encodes to bytes that decode to the
//! same value; `split_record` always makes progress and never hands out more than the cap.
#![no_main]

use id_effect_fuzz::Tree;
use id_effect_rpc::codec::postcard::{
  MAX_DEPTH, MAX_FRAME_BYTES, RECORD_HEADER_BYTES, decode_call, decode_exit, decode_frame,
  decode_limited, decode_result, encode_call, encode_exit, encode_frame, encode_result,
  split_record,
};
use id_effect_rpc::transport::frame::validate;
use libfuzzer_sys::fuzz_target;

fn frame(bytes: &[u8]) {
  let Ok(frame) = decode_frame(bytes) else {
    return;
  };
  let _ = validate(&frame);
  let again = encode_frame(&frame).expect("a decoded frame encodes");
  assert_eq!(
    decode_frame(&again).expect("re-decodes"),
    frame,
    "frame changed in a round trip"
  );
}

fuzz_target!(|data: &[u8]| {
  let Some((&selector, input)) = data.split_first() else {
    return;
  };
  match selector % 7 {
    0 => frame(input),
    1 => {
      if let Ok(call) = decode_call(input) {
        let again = encode_call(&call).expect("a decoded call encodes");
        assert_eq!(decode_call(&again).expect("re-decodes"), call);
      }
    }
    2 => {
      if let Ok(result) = decode_result(input) {
        // The failure is JSON text inside: JSON numbers may change spelling (and grow) when
        // re-encoded, so only a re-encoding within the cap is re-decoded and the values are
        // not compared.
        let Ok(again) = encode_result(&result) else {
          return;
        };
        let back = decode_result(&again).expect("re-decodes");
        assert_eq!((back.id, &back.tag), (result.id, &result.tag));
      }
    }
    3 => {
      if let Ok(exit) = decode_exit(input) {
        let Ok(again) = encode_exit(&exit) else {
          return;
        };
        let back = decode_exit(&again).expect("re-decodes");
        assert_eq!(back.is_success(), exit.is_success());
      }
    }
    4 => {
      let mut buf = input;
      while let Ok(Some((record, used))) = split_record(buf) {
        assert!(record.len() <= MAX_FRAME_BYTES, "record over the cap");
        assert_eq!(used, RECORD_HEADER_BYTES + record.len());
        frame(record);
        buf = &buf[used..];
      }
    }
    5 => {
      if let Ok(tree) = decode_limited::<Tree>(input, MAX_FRAME_BYTES) {
        assert!(tree.depth() <= MAX_DEPTH, "nesting over MAX_DEPTH");
      }
    }
    _ => {
      if let Ok(units) = decode_limited::<Vec<()>>(input, MAX_FRAME_BYTES) {
        // The element budget: 8 per input byte plus 1,024 slack (codec::postcard).
        assert!(
          units.len() <= input.len() * 8 + 1024,
          "more elements than the input's budget"
        );
      }
    }
  }
});
