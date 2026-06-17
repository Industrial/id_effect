//! Capability and runtime errors.

use super::id::CapabilityId;
use std::fmt;

/// Missing or invalid capability in an environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityError {
  /// No provider registered for this capability.
  Missing(CapabilityId),
  /// Two providers supply the same capability.
  Conflicting {
    /// Capability id.
    cap: CapabilityId,
    /// First provider name.
    first: String,
    /// Second provider name.
    second: String,
  },
}

impl fmt::Display for CapabilityError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Missing(id) => write!(f, "missing capability {id:?}"),
      Self::Conflicting { cap, first, second } => {
        write!(f, "conflicting providers for {cap:?}: {first} and {second}")
      }
    }
  }
}

impl std::error::Error for CapabilityError {}

/// Provider construction failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderError {
  /// Provider type name.
  pub provider: &'static str,
  /// Reason.
  pub message: String,
}

impl fmt::Display for ProviderError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "provider {} failed: {}", self.provider, self.message)
  }
}

impl std::error::Error for ProviderError {}

/// Planner failure (mirrors legacy layer graph codes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityPlannerError {
  /// Duplicate provider id.
  DuplicateProviderId {
    /// Id string.
    id: String,
  },
  /// Two providers for same capability.
  ConflictingProvider {
    /// Capability display name.
    cap: String,
    /// First provider id.
    first: String,
    /// Second provider id.
    second: String,
  },
  /// Required capability has no provider.
  MissingProvider {
    /// Dependent provider id.
    provider: String,
    /// Missing capability name.
    cap: String,
  },
  /// Cycle in provider dependency graph.
  CycleDetected {
    /// Involved provider ids.
    nodes: Vec<String>,
  },
}

impl CapabilityPlannerError {
  /// Stable diagnostic code.
  #[inline]
  pub fn code(&self) -> &'static str {
    match self {
      Self::DuplicateProviderId { .. } => "duplicate-provider-id",
      Self::ConflictingProvider { .. } => "conflicting-provider",
      Self::MissingProvider { .. } => "missing-provider",
      Self::CycleDetected { .. } => "cycle-detected",
    }
  }
}

/// Human-readable diagnostic for tooling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityDiagnostic {
  /// Stable code.
  pub code: &'static str,
  /// What went wrong.
  pub message: String,
  /// Fix hint.
  pub suggestion: String,
}

impl CapabilityPlannerError {
  /// Map to [`CapabilityDiagnostic`].
  pub fn to_diagnostic(&self) -> CapabilityDiagnostic {
    match self {
      Self::DuplicateProviderId { id } => CapabilityDiagnostic {
        code: self.code(),
        message: format!("Duplicate provider id `{id}`."),
        suggestion: String::from("Ensure each provider has a unique id."),
      },
      Self::ConflictingProvider { cap, first, second } => CapabilityDiagnostic {
        code: self.code(),
        message: format!("Multiple providers for capability `{cap}` (`{first}`, `{second}`)."),
        suggestion: String::from("Provide one canonical implementation per capability."),
      },
      Self::MissingProvider { provider, cap } => CapabilityDiagnostic {
        code: self.code(),
        message: format!("Provider `{provider}` requires `{cap}` but none is registered."),
        suggestion: String::from("Add a provider for the missing capability."),
      },
      Self::CycleDetected { nodes } => CapabilityDiagnostic {
        code: self.code(),
        message: format!("Provider dependency cycle: {}.", nodes.join(" -> ")),
        suggestion: String::from("Break the cycle by extracting shared deps upstream."),
      },
    }
  }
}

/// Effect execution failed at the runtime boundary.
#[derive(Debug)]
pub enum RunError<E> {
  /// Environment / graph error.
  Capability(CapabilityError),
  /// Planner error.
  Planner(CapabilityPlannerError),
  /// Effect returned `Err`.
  Effect(E),
}

impl<E: fmt::Display> fmt::Display for RunError<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Capability(e) => write!(f, "{e}"),
      Self::Planner(e) => write!(f, "{e:?}"),
      Self::Effect(e) => write!(f, "{e}"),
    }
  }
}

impl<E: std::error::Error + 'static> std::error::Error for RunError<E> {
  fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
    match self {
      Self::Capability(e) => Some(e),
      Self::Effect(e) => Some(e),
      Self::Planner(_) => None,
    }
  }
}
