//! [`ContainerImage`]: an image as an effect resource — `start`, `acquire` into a scope, `layer`.

use id_effect::{Effect, HCons, HNil, Layer, Scope, hlist};

use super::spec::start;
use super::{Container, ContainerError, ContainerSpec};

/// An image that starts as a typed running value (`PostgresContainer`, `RedisContainer`, … or
/// the plain [`Container`] of a [`ContainerSpec`]).
///
/// The provided methods make it an effect resource:
///
/// - [`layer`](Self::layer) — a scoped [`Layer`] providing [`Running`](Self::Running): started
///   when the layer is built, removed when the build's scope closes (after the effect a
///   `provide_layer` ran, however it ended: success, failure, defect or interrupt);
/// - [`acquire`](Self::acquire) — started now, removed when the given [`Scope`] closes;
/// - [`start`](Self::start) — started now, removed by [`Container::remove`] (or when its last
///   clone is dropped).
///
/// Starting is uninterruptible in `layer` and `acquire` (an interrupt waits for the container to
/// be ready or to fail its readiness timeout, then releases it), so a started container is
/// always registered for removal.
pub trait ContainerImage: Clone + Send + Sync + 'static {
  /// What a started container of this image provides: connection details and the
  /// [`Container`].
  type Running: Clone + Send + Sync + 'static;

  /// The container to start.
  fn spec(&self) -> ContainerSpec;

  /// The running value for a started, ready `container` of [`spec`](Self::spec).
  fn running(&self, container: Container) -> Self::Running;

  /// The container inside a running value.
  fn container(running: &Self::Running) -> &Container;

  /// Starts the container and waits until it is ready. The caller removes it
  /// ([`Container::remove`]); prefer [`layer`](Self::layer) or [`acquire`](Self::acquire).
  fn start<E, R>(&self) -> Effect<Self::Running, E, R>
  where
    E: From<ContainerError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    let image = self.clone();
    super::docker::docker_effect(move || async move {
      let container = start(image.spec()).await?;
      Ok(image.running(container))
    })
  }

  /// Starts the container and registers its removal in `scope`, uninterruptibly as one step.
  fn acquire<E, R>(&self, scope: &Scope) -> Effect<Self::Running, E, R>
  where
    E: From<ContainerError> + Send + 'static,
    R: Send + Sync + 'static,
  {
    let scope = scope.clone();
    self
      .start::<E, R>()
      .flat_map(move |running| {
        let release = Self::container(&running).release();
        scope.add_finalizer(release).map(move |()| running)
      })
      .uninterruptible()
  }

  /// The scoped layer providing [`Running`](Self::Running) (Effect.ts
  /// `Layer.scoped(Effect.acquireRelease(..))`).
  fn layer<E>(&self) -> Layer<HCons<Self::Running, HNil>, E>
  where
    E: From<ContainerError> + Send + 'static,
  {
    let image = self.clone();
    Layer::acquire_release(
      move || image.start().map(|running| hlist![running]),
      |env: HCons<Self::Running, HNil>, _| Self::container(&env.0).release(),
    )
  }
}

impl ContainerImage for ContainerSpec {
  type Running = Container;

  fn spec(&self) -> ContainerSpec {
    self.clone()
  }

  fn running(&self, container: Container) -> Container {
    container
  }

  fn container(running: &Container) -> &Container {
    running
  }
}
