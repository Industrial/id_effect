//! Effect + config environment for the `cli_minimal` example (see mdBook CLI chapter).

use id_effect::next::compat::IntoV1With;
use id_effect::{Effect, run_blocking, succeed};
use id_effect_config::{Config, ConfigEnv, ConfigError, MapConfigProvider, Secret, config_env};

/// The `API_TOKEN` secret as a v1 effect over [`ConfigEnv`].
///
/// m-config: `Config::run` is a `next` effect; bridged to v1 until this example switches.
fn token_secret() -> Effect<Secret<String>, ConfigError, ConfigEnv> {
  Config::string("API_TOKEN")
    .secret()
    .run::<Secret<String>, ConfigError, ConfigEnv, _>()
    .into_v1_with(|env: &mut ConfigEnv| env.clone())
}

/// Build the program [`Effect`] and the matching [`ConfigEnv`] from a token string.
///
/// The token is injected as `API_TOKEN` for [`Config::string`] + [`.secret()`](Config::secret).
pub fn app_effect(token: String) -> (Effect<(), ConfigError, ConfigEnv>, ConfigEnv) {
  let provider = MapConfigProvider::from_pairs([("API_TOKEN", token.as_str())]);
  let env = config_env(provider);
  let load = token_secret();
  let eff = load.flat_map(|_secret| succeed::<(), ConfigError, ConfigEnv>(()));
  (eff, env)
}

/// Load secret synchronously (mirrors the mdBook “parse then `run_blocking`” snippet).
pub fn load_token_secret(token: &str) -> Result<Secret<String>, ConfigError> {
  let provider = MapConfigProvider::from_pairs([("API_TOKEN", token)]);
  let env = config_env(provider);
  run_blocking(token_secret(), env)
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Exit, run_test};

  mod app_effect {
    use super::*;

    #[test]
    fn succeeds_when_token_matches_provider() {
      let (eff, env) = app_effect("s3cr3t".into());
      let exit = run_test(eff, env);
      assert!(matches!(exit, Exit::Success(())));
    }
  }

  mod load_token_secret {
    use super::*;

    #[test]
    fn returns_secret_for_configured_key() {
      let s = load_token_secret("abc").expect("load");
      assert_eq!(*s.expose(), "abc");
    }
  }
}
