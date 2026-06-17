//! Capability DI v2 providers for [`ConfigProviderService`].

use std::sync::Arc;

use ::figment::Figment;
use ::id_effect::{
  CapabilityId, CapabilityKey, Env, ProviderBox, ProviderError, ProviderNode, ProviderSpec,
};

use crate::provider::{
  ConfigProvider, ConfigProviderKey, ConfigProviderService, EnvConfigProvider,
  FigmentConfigProvider, ProviderOptions,
};

/// `std::env` [`ConfigProvider`] with default [`ProviderOptions`].
pub struct EnvConfigProviderLive;

impl ProviderSpec for EnvConfigProviderLive {
  type Key = ConfigProviderKey;
  type Output = ConfigProviderService;

  fn provider_id() -> &'static str {
    "env-config-provider"
  }

  fn provide(_deps: &Env) -> Result<ConfigProviderService, ProviderError> {
    Ok(ConfigProviderService(Arc::new(
      EnvConfigProvider::from_env(),
    )))
  }
}

/// Register `provider` as the [`ConfigProviderKey`] capability.
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
      ConfigProviderKey::id()
    }

    fn cap_name(&self) -> &str {
      "ConfigProviderKey"
    }

    fn build(&self, deps: &Env) -> Result<Env, ProviderError> {
      let mut out = deps.clone();
      out.insert::<ConfigProviderKey>(ConfigProviderService(Arc::clone(&self.0)));
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
