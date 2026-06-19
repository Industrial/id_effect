//! Graph planning errors.

/// Failure while building or sorting a dependency graph.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
  /// Two nodes share the same id.
  #[error("duplicate node id `{id}`")]
  DuplicateNode {
    /// Duplicate id.
    id: String,
  },
  /// A node requires a capability with no provider.
  #[error("node `{node}` requires missing dependency `{dependency}`")]
  MissingDependency {
    /// Dependent node id.
    node: String,
    /// Required name with no provider.
    dependency: String,
  },
  /// Two nodes provide the same capability name.
  #[error("conflicting providers for `{capability}`: `{first}` and `{second}`")]
  ConflictingProvider {
    /// Capability name.
    capability: String,
    /// First provider id.
    first: String,
    /// Second provider id.
    second: String,
  },
  /// Cycle detected during topological sort.
  #[error("cycle detected among nodes: {nodes:?}")]
  CycleDetected {
    /// Nodes still on the cycle.
    nodes: Vec<String>,
  },
  /// Edge references a node that was never registered.
  #[error("unknown node `{id}`")]
  UnknownNode {
    /// Missing node id.
    id: String,
  },
}

impl GraphError {
  /// Human-readable summary (mirrors [`Display`](std::fmt::Display)).
  pub fn message(&self) -> String {
    self.to_string()
  }
}

#[cfg(test)]
mod graph_error_tests {
  use super::*;

  #[test]
  fn message_matches_display_for_all_variants() {
    let cases: Vec<(GraphError, &str)> = vec![
      (
        GraphError::DuplicateNode { id: "x".into() },
        "duplicate node id `x`",
      ),
      (
        GraphError::MissingDependency {
          node: "repo".into(),
          dependency: "db".into(),
        },
        "node `repo` requires missing dependency `db`",
      ),
      (
        GraphError::ConflictingProvider {
          capability: "Cap".into(),
          first: "a".into(),
          second: "b".into(),
        },
        "conflicting providers for `Cap`: `a` and `b`",
      ),
      (
        GraphError::CycleDetected {
          nodes: vec!["a".into(), "b".into()],
        },
        "cycle detected among nodes: [\"a\", \"b\"]",
      ),
      (
        GraphError::UnknownNode {
          id: "missing".into(),
        },
        "unknown node `missing`",
      ),
    ];
    for (err, expected) in cases {
      assert_eq!(err.message(), expected);
      assert_eq!(err.to_string(), expected);
    }
  }
}
