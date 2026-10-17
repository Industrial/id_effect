//! The crate's Docker runtime, Docker detection ([`docker_available`], [`require_docker`]) and
//! [`container_exists`].
//!
//! Every Docker call runs on one multi-threaded tokio runtime owned by this crate, whatever
//! executor runs the effect that asked for it (`Effect::block_on`, `TokioRuntime`, a
//! `#[tokio::test]` of either flavour). The Docker client's connections and the containers'
//! drop-time cleanup therefore never depend on a test's own runtime, which ends with the test,
//! while a [`Shared`](super::Shared) container outlives it.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bollard::query_parameters::{ListContainersOptions, RemoveContainerOptions};
use id_effect::{Effect, from_async};
use testcontainers::GenericImage;
use testcontainers::{ContainerAsync, core::client::docker_client_instance};
use tokio::runtime::{Builder, Runtime};

use super::ContainerError;

/// `ID_EFFECT_REQUIRE_DOCKER`: when `1`, [`require_docker`] fails the test instead of skipping
/// it (the CI `containers-integration` job sets it).
pub const REQUIRE_DOCKER_ENV: &str = "ID_EFFECT_REQUIRE_DOCKER";

/// The label every container started by this crate carries, valued with this process's session
/// id: the reaper removes the session's containers after the process exits.
pub(crate) const SESSION_LABEL: &str = "org.id_effect.test.session";

/// The label naming one start attempt (`<session>-<n>`): a start that fails after Docker created
/// the container leaves it behind without telling its id, so it is found and removed by this.
pub(crate) const ATTEMPT_LABEL: &str = "org.id_effect.test.attempt";

/// Name of the Docker runtime's threads (also how a drop knows it runs on one).
const THREAD_NAME: &str = "id-effect-test-docker";

/// How long a ping may take before Docker counts as unavailable.
const PING_TIMEOUT: Duration = Duration::from_secs(10);

/// How long dropping a never-removed [`Container`](super::Container) waits for its removal.
const DROP_REMOVE_WAIT: Duration = Duration::from_secs(30);

/// This process's session id (`<pid>-<start nanos>`), the value of [`SESSION_LABEL`].
pub(crate) static SESSION: LazyLock<String> = LazyLock::new(|| {
  let nanos = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |d| d.as_nanos());
  format!("{}-{nanos:x}", std::process::id())
});

/// A fresh value for [`ATTEMPT_LABEL`], unique in this process and across processes.
pub(crate) fn attempt_id() -> String {
  static NEXT: AtomicU64 = AtomicU64::new(0);
  format!("{}-{}", *SESSION, NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Force-removes every container labelled [`ATTEMPT_LABEL`]`=attempt` (best effort: a failure
/// only leaves a never-started container to the reaper or `docker container prune`).
pub(crate) async fn remove_attempt(attempt: &str) {
  let Ok(docker) = docker_client_instance().await else {
    return;
  };
  let filters = HashMap::from([(
    "label".to_owned(),
    vec![format!("{ATTEMPT_LABEL}={attempt}")],
  )]);
  let list = ListContainersOptions {
    all: true,
    filters: Some(filters),
    ..Default::default()
  };
  let Ok(containers) = docker.list_containers(Some(list)).await else {
    return;
  };
  for id in containers.into_iter().filter_map(|c| c.id) {
    let remove = RemoveContainerOptions {
      force: true,
      v: true,
      ..Default::default()
    };
    let _ = docker.remove_container(&id, Some(remove)).await;
  }
}

fn runtime() -> &'static Runtime {
  static RUNTIME: OnceLock<Runtime> = OnceLock::new();
  RUNTIME.get_or_init(|| {
    Builder::new_multi_thread()
      .worker_threads(2)
      .thread_name(THREAD_NAME)
      .enable_all()
      .build()
      .expect("id_effect_test: the Docker runtime could not start")
  })
}

/// Runs `work` on the Docker runtime and awaits it from any executor.
pub(crate) async fn on_docker<T: Send + 'static>(
  work: impl Future<Output = Result<T, ContainerError>> + Send + 'static,
) -> Result<T, ContainerError> {
  match runtime().spawn(work).await {
    Ok(result) => result,
    Err(join) => Err(ContainerError::DockerUnavailable {
      reason: format!("the Docker task failed: {join}"),
    }),
  }
}

/// An effect running `work()` on the Docker runtime, its error lifted into `E`.
pub(crate) fn docker_effect<T, E, R, F, Fut>(work: F) -> Effect<T, E, R>
where
  T: Send + 'static,
  E: From<ContainerError> + Send + 'static,
  R: Send + Sync + 'static,
  F: FnOnce() -> Fut + Send + 'static,
  Fut: Future<Output = Result<T, ContainerError>> + Send + 'static,
{
  from_async(move || async move { on_docker(work()).await.map_err(E::from) })
}

/// Runs `work` on the Docker runtime and blocks this thread until it is done. Never called from
/// a Docker runtime thread.
fn block_on_docker<T: Send + 'static>(
  work: impl Future<Output = Result<T, ContainerError>> + Send + 'static,
) -> Result<T, ContainerError> {
  let (tx, rx) = mpsc::channel();
  runtime().spawn(async move {
    let _ = tx.send(work.await);
  });
  match rx.recv() {
    Ok(result) => result,
    Err(_) => Err(ContainerError::DockerUnavailable {
      reason: "the Docker task failed".into(),
    }),
  }
}

/// Pings the Docker daemon testcontainers is configured for (`DOCKER_HOST`,
/// `~/.testcontainers.properties`, the default socket).
pub(crate) async fn ping() -> Result<(), ContainerError> {
  let reason = match docker_client_instance().await {
    Err(e) => e.to_string(),
    Ok(docker) => match tokio::time::timeout(PING_TIMEOUT, docker.ping()).await {
      Ok(Ok(_)) => return Ok(()),
      Ok(Err(e)) => e.to_string(),
      Err(_) => format!("no answer to a ping within {PING_TIMEOUT:?}"),
    },
  };
  Err(ContainerError::DockerUnavailable { reason })
}

/// Whether the Docker daemon answers a ping. Probed once per process; later calls return the
/// first answer. Blocks the calling thread for the probe (use it at the top of a test, not
/// inside a running effect).
pub fn docker_available() -> Result<(), ContainerError> {
  static PROBE: OnceLock<Result<(), ContainerError>> = OnceLock::new();
  PROBE.get_or_init(|| block_on_docker(ping())).clone()
}

/// The standard container-test guard: `true` when Docker is available. Without Docker it prints
/// `SKIP <test>: <reason>` to stderr and returns `false` (the test returns early and passes),
/// unless `ID_EFFECT_REQUIRE_DOCKER=1`, where it panics so the test fails.
///
/// ```no_run
/// // At the top of a container test:
/// if !id_effect_test::require_docker() {
///   return;
/// }
/// // … start containers …
/// ```
pub fn require_docker() -> bool {
  let required = std::env::var(REQUIRE_DOCKER_ENV).as_deref() == Ok("1");
  let thread = std::thread::current();
  decide(
    docker_available(),
    required,
    thread.name().unwrap_or("test"),
  )
}

/// [`require_docker`]'s rule, given the probe and the environment.
fn decide(probe: Result<(), ContainerError>, required: bool, test: &str) -> bool {
  match probe {
    Ok(()) => true,
    Err(e) if required => panic!("{REQUIRE_DOCKER_ENV}=1 but {e}"),
    Err(e) => {
      eprintln!("SKIP {test}: {e}");
      false
    }
  }
}

/// Whether the container `id` still exists (running or not); `false` once it was removed. For
/// asserting teardown.
pub fn container_exists<E, R>(id: impl Into<String>) -> Effect<bool, E, R>
where
  E: From<ContainerError> + Send + 'static,
  R: Send + Sync + 'static,
{
  let id = id.into();
  docker_effect(move || exists(id))
}

async fn exists(id: String) -> Result<bool, ContainerError> {
  let docker = match docker_client_instance().await {
    Ok(docker) => docker,
    Err(e) => {
      return Err(ContainerError::DockerUnavailable {
        reason: e.to_string(),
      });
    }
  };
  match docker.inspect_container(&id, None).await {
    Ok(_) => Ok(true),
    Err(bollard::errors::Error::DockerResponseServerError {
      status_code: 404, ..
    }) => Ok(false),
    Err(e) => Err(ContainerError::Docker {
      id,
      reason: e.to_string(),
    }),
  }
}

/// Removes `container` on the Docker runtime from a `Drop`: waits for the removal (bounded)
/// unless the drop itself runs on a Docker runtime thread, where waiting could starve it.
pub(crate) fn remove_detached(container: ContainerAsync<GenericImage>) {
  let (tx, rx) = mpsc::channel();
  runtime().spawn(async move {
    let _ = container.rm().await;
    let _ = tx.send(());
  });
  if std::thread::current().name() != Some(THREAD_NAME) {
    let _ = rx.recv_timeout(DROP_REMOVE_WAIT);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn available_docker_runs_the_test() {
    assert!(decide(Ok(()), true, "t"));
    assert!(decide(Ok(()), false, "t"));
  }

  #[test]
  fn missing_docker_skips_unless_required() {
    let gone = || ContainerError::DockerUnavailable {
      reason: "no socket".into(),
    };
    assert!(!decide(Err(gone()), false, "t"));
    let required = std::panic::catch_unwind(|| decide(Err(gone()), true, "t"));
    let message = *required
      .expect_err("required")
      .downcast::<String>()
      .unwrap();
    assert_eq!(
      message,
      "ID_EFFECT_REQUIRE_DOCKER=1 but Docker is not available: no socket"
    );
  }

  #[test]
  fn session_id_is_stable_and_names_the_process() {
    assert_eq!(*SESSION, *SESSION);
    assert!(SESSION.starts_with(&format!("{}-", std::process::id())));
  }
}
