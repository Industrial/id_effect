//! RPC batch bodies (id_effect_rpc `wire::decode_batch(bytes, max_calls, max_bytes)`): the JSON
//! array a client posts to `/rpc`. The first two input bytes pick the caps (calls `1..=64`, bytes
//! up to 64 KiB, so both refusals are reached), the rest is the body; a second run uses the
//! server's defaults.
//!
//! Checks besides "no panic": an accepted batch is non-empty, within both caps and has distinct
//! ids on every call; a body over a cap is refused with that cap's error before any element is
//! decoded.
#![no_main]

use std::collections::HashSet;

use id_effect_rpc::wire::{BatchError, MAX_BATCH_BYTES, MAX_BATCH_CALLS, decode_batch};
use libfuzzer_sys::fuzz_target;

fn check(body: &[u8], max_calls: usize, max_bytes: usize) {
  match decode_batch(body, max_calls, max_bytes) {
    Ok(calls) => {
      assert!(!calls.is_empty(), "an empty batch was accepted");
      assert!(calls.len() <= max_calls, "more calls than the cap");
      assert!(
        body.len() <= max_bytes,
        "a body over the byte cap was accepted"
      );
      let mut ids = HashSet::new();
      for call in &calls {
        let id = call.id.expect("every call of an accepted batch has an id");
        assert!(ids.insert(id), "call id {} used twice", id.get());
      }
    }
    Err(BatchError::TooLarge { len, max }) => {
      assert_eq!((len, max), (body.len(), max_bytes));
      assert!(len > max);
    }
    Err(BatchError::TooManyCalls { max }) => assert_eq!(max, max_calls),
    Err(_) => {}
  }
}

fuzz_target!(|data: &[u8]| {
  let [calls, bytes, body @ ..] = data else {
    return;
  };
  let max_calls = usize::from(calls % 64) + 1;
  let max_bytes = usize::from(*bytes) * 256;
  check(body, max_calls, max_bytes);
  check(body, MAX_BATCH_CALLS, MAX_BATCH_BYTES);
});
