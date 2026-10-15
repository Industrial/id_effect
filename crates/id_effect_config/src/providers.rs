//! [`Layer`]s that provide the [`ConfigProviderService`] capability (`next` kernel).
//!
//! Each layer outputs a [`ConfigEnv`] (`HCons<ConfigProviderService, HNil>`), needs nothing
//! (`In = HNil`) and never fails, so its error type `E` is whatever the graph it joins uses.

use std::sync::Arc;

use ::figment::Figment;
use ::id_effect::next::{Layer, hlist, sync};

use crate::ConfigEnv;
use crate::provider::{
  ConfigProvider, ConfigProviderService, EnvConfigProvider, FigmentConfigProvider, ProviderOptions,
};

/// `std::env` [`ConfigProvider`] with default [`ProviderOptions`].
///
/// [`EnvConfigProviderLive::layer`] reads the process environment when the layer is built.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvConfigProviderLive;

impl EnvConfigProviderLive {
  /// The layer: an [`EnvConfigProvider::from_env`] built once per layer build.
  pub fn layer<E: Send + 'static>() -> Layer<ConfigEnv, E> {
    Layer::effect(|| {
      sync(|| {
        hlist![ConfigProviderService(Arc::new(
          EnvConfigProvider::from_env()
        ))]
      })
    })
  }
}

/// A layer providing `provider` as the [`ConfigProvider`] capability.
#[inline]
pub fn provide_config_provider<P, E>(provider: P) -> Layer<ConfigEnv, E>
where
  P: ConfigProvider + Send + Sync + 'static,
  E: Send + 'static,
{
  Layer::succeed(hlist![ConfigProviderService(Arc::new(provider))])
}

/// A layer providing an [`EnvConfigProvider`] built with explicit options.
#[inline]
pub fn provide_env_config_provider<E: Send + 'static>(
  options: ProviderOptions,
) -> Layer<ConfigEnv, E> {
  provide_config_provider(EnvConfigProvider::new(options))
}

/// A layer providing a [`FigmentConfigProvider`] built from `figment`.
#[inline]
pub fn provide_figment_config_provider<E: Send + 'static>(figment: Figment) -> Layer<ConfigEnv, E> {
  provide_config_provider(FigmentConfigProvider::new(figment))
}
