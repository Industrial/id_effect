//! Structural diffs between values.

/// Difference between two compared values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Diff<T> {
  /// Value unchanged.
  Unchanged(T),
  /// Value newly present.
  Added(T),
  /// Value removed.
  Removed(T),
  /// Value replaced.
  Changed {
    /// Previous value.
    old: T,
    /// New value.
    new: T,
  },
}

impl<T> Diff<T> {
  /// Whether this diff represents no change.
  #[must_use]
  pub fn is_unchanged(&self) -> bool {
    matches!(self, Self::Unchanged(_))
  }
}

/// Compare two values with [`PartialEq`].
#[must_use]
pub fn diff_values<T: PartialEq>(old: T, new: T) -> Diff<T> {
  if old == new {
    Diff::Unchanged(old)
  } else {
    Diff::Changed { old, new }
  }
}

/// Compare optional snapshots (for insert/remove semantics).
#[must_use]
pub fn diff_option<T: PartialEq>(before: Option<T>, after: Option<T>) -> Option<Diff<T>> {
  match (before, after) {
    (None, None) => None,
    (Some(value), None) => Some(Diff::Removed(value)),
    (None, Some(value)) => Some(Diff::Added(value)),
    (Some(old), Some(new)) => {
      if old == new {
        Some(Diff::Unchanged(old))
      } else {
        Some(Diff::Changed { old, new })
      }
    }
  }
}

/// Apply a diff onto an optional base value.
#[must_use]
pub fn apply_diff<T>(base: Option<T>, diff: &Diff<T>) -> Option<T>
where
  T: Clone,
{
  match diff {
    Diff::Unchanged(value) | Diff::Added(value) | Diff::Changed { new: value, .. } => {
      Some(value.clone())
    }
    Diff::Removed(_) => None,
  }
  .or(base)
}
