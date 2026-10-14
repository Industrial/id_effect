//! [`NodeId`] — the wire-visible identity of a node.
//!
//! A `NodeId` is the logical [`NodeName`] (`name@host`) plus a 128-bit
//! random **incarnation** minted at boot. Two boots of the same logical
//! node never share an incarnation, so stale connections and stale pids
//! are always distinguishable from the live node (BEAM's creation tag,
//! widened so collisions are out of the picture).

use std::fmt;

use id_effect_process::NodeName;
use serde::{Deserialize, Serialize};

/// Wire-visible node identity: logical name + boot incarnation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId {
  name: String,
  incarnation: u128,
}

impl NodeId {
  /// Mint a fresh identity for this boot of `name`.
  ///
  /// Incarnations are **time-ordered**: the high 64 bits are boot time in
  /// nanoseconds, the low 64 bits are random. Later boots of the same
  /// logical node therefore compare strictly greater (given sane wall
  /// clocks), which is what lets a restarted node win the identity
  /// conflict against its own stale record in membership.
  pub fn fresh(name: &NodeName) -> Self {
    let nanos = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default()
      .as_nanos() as u64;
    let incarnation = ((nanos as u128) << 64) | (rand::random::<u64>() as u128);
    Self {
      name: name.as_str().to_string(),
      incarnation,
    }
  }

  /// Rebuild an identity from its parts (wire decode, tests).
  pub fn from_parts(name: impl Into<String>, incarnation: u128) -> Self {
    Self {
      name: name.into(),
      incarnation,
    }
  }

  /// The logical node name (`name@host`).
  pub fn name(&self) -> &str {
    &self.name
  }

  /// The boot incarnation.
  pub fn incarnation(&self) -> u128 {
    self.incarnation
  }

  /// Same logical node, regardless of boot.
  pub fn same_node(&self, other: &NodeId) -> bool {
    self.name == other.name
  }
}

impl fmt::Display for NodeId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}#{:x}", self.name, self.incarnation)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn fresh_keeps_the_logical_name() {
    let id = NodeId::fresh(&NodeName::new("a@host"));
    assert_eq!(id.name(), "a@host");
  }

  #[test]
  fn two_boots_never_share_an_identity() {
    let name = NodeName::new("a@host");
    let first = NodeId::fresh(&name);
    let second = NodeId::fresh(&name);
    assert_ne!(first, second);
    assert!(first.same_node(&second));
  }

  #[test]
  fn from_parts_exposes_its_parts() {
    let id = NodeId::from_parts("b@host", 42);
    assert_eq!(id.name(), "b@host");
    assert_eq!(id.incarnation(), 42);
    assert_eq!(id, NodeId::from_parts("b@host", 42));
  }

  #[test]
  fn same_node_ignores_incarnation_only() {
    let a1 = NodeId::from_parts("a@host", 1);
    assert!(a1.same_node(&NodeId::from_parts("a@host", 2)));
    assert!(!a1.same_node(&NodeId::from_parts("b@host", 1)));
  }

  #[test]
  fn display_is_name_hash_hex_incarnation() {
    assert_eq!(NodeId::from_parts("a@host", 255).to_string(), "a@host#ff");
  }

  #[test]
  fn incarnation_high_bits_are_boot_time() {
    // 2020-01-01T00:00:00Z in nanoseconds: any sane boot clock is past it.
    const JAN_2020_NANOS: u64 = 1_577_836_800_000_000_000;
    let id = NodeId::fresh(&NodeName::new("a@host"));
    assert!((id.incarnation() >> 64) as u64 > JAN_2020_NANOS);
  }

  #[test]
  fn serde_round_trip_preserves_identity() {
    let id = NodeId::from_parts("a@host", u128::MAX - 1);
    let bytes = postcard::to_allocvec(&id).expect("encode");
    let back: NodeId = postcard::from_bytes(&bytes).expect("decode");
    assert_eq!(back, id);
  }
}
