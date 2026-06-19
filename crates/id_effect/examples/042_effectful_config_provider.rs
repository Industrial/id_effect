//! Ex 042 — effectful provider builds config via [`Effect`] before the app runs.

use id_effect::{Effect, Env, Needs, ProviderError, ProviderSpec, caps, provide, run_with};

#[::id_effect::capability(String)]
#[expect(dead_code)]
struct AppConfig;

struct ConfigLoaderLive;

impl ProviderSpec for ConfigLoaderLive {
  type Key = AppConfigKey;
  type Output = String;

  fn provider_id() -> &'static str {
    "config-loader"
  }

  fn effectful_build() -> bool {
    true
  }

  fn provide(_deps: &Env) -> Result<String, ProviderError> {
    Err(ProviderError {
      provider: "ConfigLoaderLive",
      message: "use provide_effect".into(),
    })
  }

  fn provide_effect(_deps: &Env) -> Effect<String, ProviderError, Env> {
    Effect::new(|_env| Ok("loaded-from-effect".to_string()))
  }
}

fn app() -> Effect<String, (), caps!(AppConfigKey)> {
  Effect::new(|env: &mut caps!(AppConfigKey)| Ok(Needs::<AppConfigKey>::need(env).clone()))
}

fn main() {
  let cfg = run_with([provide!(ConfigLoaderLive)], app()).expect("run");
  assert_eq!(cfg, "loaded-from-effect");
  println!("042_effectful_config_provider ok: {cfg}");
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn effectful_provider_runs_before_app() {
    let env = build_env([provide!(ConfigLoaderLive)]).expect("build");
    assert_eq!(env.get::<AppConfigKey>(), "loaded-from-effect");
  }
}
