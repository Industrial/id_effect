//! Effect + config environment for the `cli_minimal` example (see mdBook CLI chapter).

use id_effect::Cause;
use id_effect::Effect;
use id_effect_config::{Config, ConfigEnv, ConfigError, MapConfigProvider, Secret, config_env};

/// The `API_TOKEN` secret, loaded from the [`ConfigEnv`] capability.
fn token_secret() -> Effect<Secret<String>, ConfigError, ConfigEnv> {
  Config::string("API_TOKEN")
    .secret()
    .run::<Secret<String>, ConfigError, ConfigEnv, _>()
}

/// Build the program [`Effect`] and the matching [`ConfigEnv`] from a token string.
///
/// The token is injected as `API_TOKEN` for [`Config::string`] + [`.secret()`](Config::secret).
pub fn app_effect(token: String) -> (Effect<(), ConfigError, ConfigEnv>, ConfigEnv) {
  let provider = MapConfigProvider::from_pairs([("API_TOKEN", token.as_str())]);
  let env = config_env(provider);
  let eff = token_secret().map(|_secret| ());
  (eff, env)
}

/// Load the secret synchronously (mirrors the mdBook “parse, then run at the edge” snippet).
pub fn load_token_secret(token: &str) -> Result<Secret<String>, Cause<ConfigError>> {
  let provider = MapConfigProvider::from_pairs([("API_TOKEN", token)]);
  let env = config_env(provider);
  #[allow(
    clippy::disallowed_methods,
    reason = "edge: a synchronous loader for callers outside any effect (the CLI's `main`)"
  )]
  let exit = token_secret().provide_env(env).block_on();
  exit.into_result()
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Exit;

  mod app_effect {
    use super::*;

    #[test]
    fn succeeds_when_token_matches_provider() {
      let (eff, env) = app_effect("s3cr3t".into());
      let exit = eff.provide_env(env).block_on();
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
