//! [`Container`]: a started, ready container — its host, published ports and removal.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use id_effect::{Effect, Never, from_async};
use testcontainers::{ContainerAsync, GenericImage};

use super::ContainerError;
use super::docker::{container_exists, docker_effect, on_docker, remove_detached};

/// A started, ready container. Cloning is an `Arc` bump; every clone is the same container.
///
/// A scoped container ([`ContainerImage::layer`](super::ContainerImage::layer),
/// [`acquire`](super::ContainerImage::acquire)) is removed when its scope closes. One started
/// with [`ContainerImage::start`](super::ContainerImage::start) is removed by
/// [`remove`](Self::remove), or at the latest when its last clone is dropped.
#[derive(Clone)]
pub struct Container {
  inner: Arc<Inner>,
}

struct Inner {
  id: String,
  image: String,
  host: String,
  /// Container port → host port, for every port the spec published.
  ports: BTreeMap<u16, u16>,
  /// `None` once removed.
  handle: Mutex<Option<ContainerAsync<GenericImage>>>,
}

impl Container {
  /// Reads the host and the published ports of `started` (on the Docker runtime). A container
  /// whose ports cannot be read is removed.
  pub(crate) async fn resolve(
    started: ContainerAsync<GenericImage>,
    image: String,
    ports: &[u16],
  ) -> Result<Self, ContainerError> {
    match published(&started, ports).await {
      Ok((host, ports)) => Ok(Self {
        inner: Arc::new(Inner {
          id: started.id().to_owned(),
          image,
          host,
          ports,
          handle: Mutex::new(Some(started)),
        }),
      }),
      Err(e) => {
        let _ = started.rm().await;
        Err(ContainerError::Start {
          image,
          reason: e.to_string(),
        })
      }
    }
  }

  /// The Docker container id.
  pub fn id(&self) -> &str {
    &self.inner.id
  }

  /// The image, `name:tag`.
  pub fn image(&self) -> &str {
    &self.inner.image
  }

  /// The host the published ports listen on (`127.0.0.1` for a local Docker daemon).
  pub fn host(&self) -> &str {
    &self.inner.host
  }

  /// The host port published for the container port `port`.
  pub fn host_port(&self, port: u16) -> Result<u16, ContainerError> {
    self
      .inner
      .ports
      .get(&port)
      .copied()
      .ok_or_else(|| ContainerError::PortNotExposed {
        image: self.inner.image.clone(),
        port,
      })
  }

  /// `host:port` for the container port `port`.
  pub fn address(&self, port: u16) -> Result<String, ContainerError> {
    self
      .host_port(port)
      .map(|host_port| format!("{}:{host_port}", self.inner.host))
  }

  /// The host port of a container port the image's own spec publishes.
  pub(crate) fn mapped_port(&self, port: u16) -> u16 {
    // Every built-in image publishes the port it asks for, and a container only exists once
    // all its published ports were read.
    self.inner.ports[&port]
  }

  /// Removes the container (stopping it first). Idempotent: removing it again, or after its
  /// scope closed, succeeds without a Docker call.
  pub fn remove<E, R>(&self) -> Effect<(), E, R>
  where
    E: From<ContainerError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    let inner = Arc::clone(&self.inner);
    docker_effect(move || remove(inner))
  }

  /// Whether the container still exists in Docker (`false` once removed).
  pub fn exists<E, R>(&self) -> Effect<bool, E, R>
  where
    E: From<ContainerError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    container_exists(self.id())
  }

  /// The scope finalizer: removes the container; a failure is reported on stderr (finalizers
  /// cannot fail).
  pub(crate) fn release(&self) -> Effect<(), Never> {
    let inner = Arc::clone(&self.inner);
    from_async(move || async move {
      if let Err(e) = on_docker(remove(inner)).await {
        eprintln!("id_effect_test: {e}");
      }
      Ok(())
    })
  }
}

async fn published(
  started: &ContainerAsync<GenericImage>,
  ports: &[u16],
) -> testcontainers::core::error::Result<(String, BTreeMap<u16, u16>)> {
  let host = loopback(started.get_host().await?.to_string());
  let mut mapped = BTreeMap::new();
  for &port in ports {
    mapped.insert(port, started.get_host_port_ipv4(port).await?);
  }
  Ok((host, mapped))
}

/// `localhost` as `127.0.0.1`: Docker publishes on IPv4 and IPv6 with possibly different ports,
/// and the IPv4 one is the port read back.
fn loopback(host: String) -> String {
  if host == "localhost" {
    "127.0.0.1".to_owned()
  } else {
    host
  }
}

async fn remove(inner: Arc<Inner>) -> Result<(), ContainerError> {
  let handle = inner
    .handle
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .take();
  let Some(handle) = handle else {
    return Ok(());
  };
  match handle.rm().await {
    Ok(()) => Ok(()),
    Err(e) => Err(ContainerError::Docker {
      id: inner.id.clone(),
      reason: e.to_string(),
    }),
  }
}

impl Drop for Inner {
  fn drop(&mut self) {
    let handle = self
      .handle
      .get_mut()
      .unwrap_or_else(PoisonError::into_inner)
      .take();
    if let Some(handle) = handle {
      remove_detached(handle);
    }
  }
}

impl fmt::Debug for Container {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Container")
      .field("id", &self.inner.id)
      .field("image", &self.inner.image)
      .field("host", &self.inner.host)
      .field("ports", &self.inner.ports)
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::loopback;

  #[test]
  fn localhost_is_read_as_the_ipv4_loopback() {
    assert_eq!(loopback("localhost".into()), "127.0.0.1");
    assert_eq!(loopback("docker.internal".into()), "docker.internal");
  }
}
