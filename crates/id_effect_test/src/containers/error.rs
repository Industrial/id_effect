//! [`ContainerError`]: why a container could not be started, reached or removed.

use std::fmt;
use std::time::Duration;

/// A container operation failed. Every variant names what it was about, so a failing test
/// reports the image (and the container id once there is one) without a backtrace.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerError {
  /// The Docker daemon did not answer a ping (no socket, daemon stopped, permission denied).
  DockerUnavailable {
    /// The client or ping error.
    reason: String,
  },
  /// Docker refused to create or start the container (unknown image, pull failure, …).
  Start {
    /// `name:tag` of the image.
    image: String,
    /// The Docker error.
    reason: String,
  },
  /// The container started but was not ready within its readiness timeout. It has been
  /// removed.
  NotReady {
    /// `name:tag` of the image.
    image: String,
    /// The removed container.
    id: String,
    /// The readiness timeout that passed.
    timeout: Duration,
  },
  /// The readiness wait failed before the timeout (the container exited, its health check
  /// reported unhealthy, its log stream ended). It has been removed.
  Unready {
    /// `name:tag` of the image.
    image: String,
    /// The removed container.
    id: String,
    /// What the wait reported.
    reason: String,
  },
  /// The container does not publish this container port.
  PortNotExposed {
    /// `name:tag` of the image.
    image: String,
    /// The container port asked for.
    port: u16,
  },
  /// Removing or inspecting a container failed.
  Docker {
    /// The container.
    id: String,
    /// The Docker error.
    reason: String,
  },
  /// The reaper (testcontainers Ryuk), which removes shared containers when the test process
  /// exits, could not be started or did not acknowledge the session.
  Reaper {
    /// What failed.
    reason: String,
  },
}

impl fmt::Display for ContainerError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::DockerUnavailable { reason } => write!(f, "Docker is not available: {reason}"),
      Self::Start { image, reason } => write!(f, "container {image} did not start: {reason}"),
      Self::NotReady { image, id, timeout } => write!(
        f,
        "container {image} ({id}) was not ready within {timeout:?}; it was removed"
      ),
      Self::Unready { image, id, reason } => write!(
        f,
        "container {image} ({id}) failed its readiness wait: {reason}; it was removed"
      ),
      Self::PortNotExposed { image, port } => {
        write!(f, "container {image} does not expose port {port}")
      }
      Self::Docker { id, reason } => write!(f, "Docker call on container {id} failed: {reason}"),
      Self::Reaper { reason } => write!(f, "container reaper: {reason}"),
    }
  }
}

impl std::error::Error for ContainerError {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_variant_names_its_subject() {
    let cases = [
      (
        ContainerError::DockerUnavailable {
          reason: "no socket".into(),
        },
        "Docker is not available: no socket",
      ),
      (
        ContainerError::Start {
          image: "redis:7".into(),
          reason: "404".into(),
        },
        "container redis:7 did not start: 404",
      ),
      (
        ContainerError::NotReady {
          image: "redis:7".into(),
          id: "abc".into(),
          timeout: Duration::from_secs(2),
        },
        "container redis:7 (abc) was not ready within 2s; it was removed",
      ),
      (
        ContainerError::Unready {
          image: "redis:7".into(),
          id: "abc".into(),
          reason: "exited".into(),
        },
        "container redis:7 (abc) failed its readiness wait: exited; it was removed",
      ),
      (
        ContainerError::PortNotExposed {
          image: "redis:7".into(),
          port: 1,
        },
        "container redis:7 does not expose port 1",
      ),
      (
        ContainerError::Docker {
          id: "abc".into(),
          reason: "gone".into(),
        },
        "Docker call on container abc failed: gone",
      ),
      (
        ContainerError::Reaper {
          reason: "no ACK".into(),
        },
        "container reaper: no ACK",
      ),
    ];
    for (error, text) in cases {
      assert_eq!(error.to_string(), text);
      let _: &dyn std::error::Error = &error;
    }
  }
}
