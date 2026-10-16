//! [`live_layer`]: the configuration from the process environment.

use id_effect::{Layer, caps};
use id_effect_config::EnvConfigProviderLive;

use super::StartupError;
use crate::infrastructure::config::AppConfig;

/// [`AppConfig`] loaded from the process environment ([`EnvConfigProviderLive`]); a bad key
/// fails the build with [`StartupError::Config`].
pub fn live_layer() -> Layer<caps!(AppConfig), StartupError> {
  EnvConfigProviderLive::layer().provide_to(AppConfig::layer())
}
