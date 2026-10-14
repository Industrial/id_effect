//! Control-plane introspection types (Axum mounts these as JSON).
//!
//! Kept dependency-free so `id_effect_axum` (or any HTTP stack) can serialize
//! them. Admin actions (`drain`, `rebalance`) are represented as request DTOs.

use serde::{Deserialize, Serialize};

/// Cluster membership view for `/cluster/members`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemberView {
  /// Logical node name.
  pub name: String,
  /// Boot incarnation (hex).
  pub incarnation: String,
  /// Dial address if known.
  pub addr: Option<String>,
}

/// Process table row for `/cluster/processes`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessView {
  /// Display pid.
  pub pid: String,
  /// Hosting node.
  pub node: String,
  /// Registered names, if any.
  pub names: Vec<String>,
}

/// Supervision-tree node for introspection.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TreeNodeView {
  /// Child id or process display name.
  pub id: String,
  /// Pid if running.
  pub pid: Option<String>,
  /// Nested children.
  pub children: Vec<TreeNodeView>,
}

/// Admin: drain this node (stop accepting work, migrate children).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DrainRequest {
  /// Grace period seconds before brutal kill.
  pub grace_secs: u64,
}

/// Admin: trigger Fabric rebalance / ScaleOut pass.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RebalanceRequest {
  /// Optional target node; omit for automatic placement.
  pub target_node: Option<String>,
}

/// Bundle of cluster introspection snapshots.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClusterStatus {
  /// Members.
  pub members: Vec<MemberView>,
  /// Local processes.
  pub processes: Vec<ProcessView>,
  /// Optional supervision tree root.
  pub tree: Option<TreeNodeView>,
  /// Whether this node is draining.
  pub draining: bool,
}

impl ClusterStatus {
  /// Empty status.
  pub fn new() -> Self {
    Self::default()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn new_status_is_empty_and_not_draining() {
    let status = ClusterStatus::new();
    assert!(status.members.is_empty());
    assert!(status.processes.is_empty());
    assert!(status.tree.is_none());
    assert!(!status.draining);
  }

  #[test]
  fn status_survives_a_serde_round_trip() {
    let status = ClusterStatus {
      members: vec![MemberView {
        name: "a@host".into(),
        incarnation: "ff".into(),
        addr: Some("10.0.0.1:4000".into()),
      }],
      processes: vec![ProcessView {
        pid: "<a@host.1.1>".into(),
        node: "a@host".into(),
        names: vec!["counter".into()],
      }],
      tree: Some(TreeNodeView {
        id: "root".into(),
        pid: None,
        children: vec![TreeNodeView {
          id: "w1".into(),
          pid: Some("<a@host.2.1>".into()),
          children: Vec::new(),
        }],
      }),
      draining: true,
    };
    let bytes = postcard::to_allocvec(&status).expect("encode");
    let back: ClusterStatus = postcard::from_bytes(&bytes).expect("decode");
    assert_eq!(back.members[0].addr.as_deref(), Some("10.0.0.1:4000"));
    assert_eq!(back.processes[0].names, vec!["counter".to_string()]);
    let tree = back.tree.expect("tree");
    assert_eq!(tree.children[0].pid.as_deref(), Some("<a@host.2.1>"));
    assert!(back.draining);
  }

  #[test]
  fn admin_requests_round_trip() {
    let drain = DrainRequest { grace_secs: 30 };
    let bytes = postcard::to_allocvec(&drain).expect("encode");
    let back: DrainRequest = postcard::from_bytes(&bytes).expect("decode");
    assert_eq!(back.grace_secs, 30);

    let rebalance = RebalanceRequest {
      target_node: Some("b@host".into()),
    };
    let bytes = postcard::to_allocvec(&rebalance).expect("encode");
    let back: RebalanceRequest = postcard::from_bytes(&bytes).expect("decode");
    assert_eq!(back.target_node.as_deref(), Some("b@host"));
  }
}
