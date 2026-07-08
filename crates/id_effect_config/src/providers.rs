//! Capability DI v2 providers for [`ConfigProviderService`].

#![allow(clippy::new_ret_no_self, clippy::unused_unit)]

use std::sync::Arc;

use ::figment::Figment;
use ::id_effect::{
  Cap, CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode,
};

use crate::provider::{
  ConfigProvider, ConfigProviderService, EnvConfigProvider, FigmentConfigProvider, ProviderOptions,
};

/// `std::env` [`ConfigProvider`] with default [`ProviderOptions`].
#[derive(::id_effect::ProviderSpecDerive)]
#[provides(ConfigProviderService)]
pub struct EnvConfigProviderLive;

impl EnvConfigProviderLive {
  fn new() -> ConfigProviderService {
    ConfigProviderService(Arc::new(EnvConfigProvider::from_env()))
  }
}

/// Register `provider` as the [`ConfigProvider`] capability.
#[inline]
pub fn provide_config_provider<P>(provider: P) -> ProviderBox
where
  P: ConfigProvider + Send + Sync + 'static,
{
  struct Node(Arc<dyn ConfigProvider>);

  impl ProviderNode for Node {
    fn id(&self) -> &str {
      "config-provider"
    }

    fn requires(&self) -> &[CapabilityId] {
      &[]
    }

    fn provides(&self) -> CapabilityId {
      Cap::<ConfigProviderService>::id()
    }

    fn cap_name(&self) -> &str {
      "ConfigProvider"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<Cap<ConfigProviderService>>(ConfigProviderService(Arc::clone(&self.0)));
      Ok(out)
    }
  }

  ProviderBox(Arc::new(Node(Arc::new(provider))))
}

/// Register an [`EnvConfigProvider`] built with explicit options.
#[inline]
pub fn provide_env_config_provider(options: ProviderOptions) -> ProviderBox {
  provide_config_provider(EnvConfigProvider::new(options))
}

/// Register a [`FigmentConfigProvider`] built from `figment`.
#[inline]
pub fn provide_figment_config_provider(figment: Figment) -> ProviderBox {
  provide_config_provider(FigmentConfigProvider::new(figment))
}
