//! Mesh process envelopes (id_effect_node `decode_envelope`) handled by a receiving node: the
//! decoder at the trust boundary, then `Mesh::handle_envelope_from` (dispatch, the at-least-once
//! dedup window and acks) on a fresh node with no processes and no reachable peers.
//!
//! Checks besides "no panic": a decoded envelope re-encodes to bytes that decode to the same
//! envelope, and a reliable message unwraps to the matching at-most-once envelope.
#![no_main]

use id_effect_node::{
  BehaviorRegistry, Mesh, NodeId, ProcessEnvelope, decode_envelope, encode_envelope, traceparent_in,
};
use id_effect_process::{NodeName, ProcessNode};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
  let Ok(envelope) = decode_envelope(data) else {
    return;
  };
  let bytes = encode_envelope(&envelope).expect("a decoded envelope encodes");
  assert_eq!(
    decode_envelope(&bytes).expect("re-decodes"),
    envelope,
    "envelope changed in a round trip"
  );
  if let ProcessEnvelope::Reliable { message, .. } = &envelope {
    let _ = message.payload_len();
    let to = message.to().clone();
    match message.clone().into_envelope() {
      ProcessEnvelope::Cast { to: t, trace, .. } | ProcessEnvelope::Info { to: t, trace, .. } => {
        assert_eq!(t, to);
        let _ = traceparent_in(&trace);
      }
      other => panic!("unwrapped to {other:?}"),
    }
  }
  let name = NodeName::new("fuzz@local");
  let mesh = Mesh::new(
    NodeId::from_parts("fuzz@local", 1),
    ProcessNode::new(name),
    BehaviorRegistry::new(),
  );
  let peer = NodeId::from_parts("peer@remote", 2);
  let _ = mesh.handle_envelope_from(&peer, envelope.clone());
  // The same frame twice: a reliable one is a duplicate the second time.
  let _ = mesh.handle_envelope_from(&peer, envelope);
});
