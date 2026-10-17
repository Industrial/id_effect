//! [`Shared`]: one container per test binary, started on first use and reused by every test.

use id_effect::{Effect, HCons, HNil, Layer, from_async, hlist};
use tokio::sync::OnceCell;

use super::docker::on_docker;
use super::spec::start;
use super::{ContainerError, ContainerImage, reaper};

/// A container shared by every test of one test binary (one process): a `static` started
/// lazily by the first test that asks for it, then handed to every later test as is. Start-up
/// cost is paid once; tests isolate their data themselves (a schema or database per test, a
/// key prefix, a rolled-back transaction).
///
/// The first start also starts the reaper ([module docs](super#shared-containers)), which
/// removes the container after the process exits. A failed start is remembered: every later
/// test fails with the same [`ContainerError`] instead of retrying.
///
/// Under `cargo nextest` each test runs in its own process, so "per binary" is "per test" there;
/// under `cargo test` the tests of a binary share it.
///
/// ```no_run
/// use id_effect::{Effect, Exit, caps, need};
/// use id_effect_test::{ContainerError, Postgres, PostgresContainer, Shared};
///
/// static PG: Shared<Postgres> = Shared::new(Postgres::new);
///
/// // In every test of the binary: the same container.
/// let url: Effect<String, ContainerError, caps!(PostgresContainer)> =
///   need::<PostgresContainer>().map(|pg| pg.url());
/// let Exit::Success(url) = url.provide_layer(PG.layer()).block_on() else {
///   panic!("no PostgreSQL")
/// };
/// assert!(url.starts_with("postgres://"));
/// ```
pub struct Shared<I: ContainerImage> {
  make: fn() -> I,
  cell: OnceCell<Result<I::Running, ContainerError>>,
}

impl<I: ContainerImage> Shared<I> {
  /// A shared container of the image `make()` builds (called once, on first use).
  pub const fn new(make: fn() -> I) -> Self {
    Self {
      make,
      cell: OnceCell::const_new(),
    }
  }

  /// The running container: started (with the reaper) on the first call, the same value on
  /// every later one. Concurrent first calls start it once.
  pub fn get<E, R>(&'static self) -> Effect<I::Running, E, R>
  where
    E: From<ContainerError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    from_async(move || async move {
      let first = || async {
        let image = (self.make)();
        on_docker(async move {
          reaper::ensure().await?;
          let container = start(image.spec()).await?;
          Ok(image.running(container))
        })
        .await
      };
      self.cell.get_or_init(first).await.clone().map_err(E::from)
    })
  }

  /// A layer providing the shared container. Building it does not start a second container,
  /// and closing its scope does not remove the shared one.
  pub fn layer<E>(&'static self) -> Layer<HCons<I::Running, HNil>, E>
  where
    E: From<ContainerError> + Send + 'static,
  {
    Layer::effect(move || self.get().map(|running| hlist![running]))
  }
}

impl<I: ContainerImage> std::fmt::Debug for Shared<I> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Shared")
      .field("image", &std::any::type_name::<I>())
      .field("started", &self.cell.initialized())
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::containers::{ContainerSpec, Redis};

  #[test]
  fn a_shared_container_starts_nothing_until_asked() {
    let shared = Shared::new(Redis::new);
    assert!(format!("{shared:?}").contains("started: false"));
    let spec = ContainerSpec::new("redis", "7-alpine").port(6379);
    assert_eq!(ContainerImage::spec(&spec).ports(), [6379]);
  }
}
