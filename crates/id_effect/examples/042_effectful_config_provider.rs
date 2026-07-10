//! Ex 042 — effectful provider builds config via [`Effect`] before the app runs.

use id_effect::{Cap, Effect, Env, Needs, ProviderError, ProviderSpec, caps, provide, run_with};

type AppConfig = String;

struct ConfigLoaderLive;

impl ProviderSpec for ConfigLoaderLive {
  type Key = Cap<AppConfig>;
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

fn app() -> Effect<String, (), caps!(AppConfig)> {
  Effect::new(|env: &mut caps!(AppConfig)| Ok(Needs::<AppConfig>::need(env).clone()))
}

fn main() {
  let cfg = run_with([provide!(ConfigLoaderLive)], app()).expect("run");
  assert_eq!(cfg, "loaded-from-effect");
  println!("042_effectful_config_provider ok: {cfg}");
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::build_env;

  #[test]
  fn effectful_provider_runs_before_app() {
    let env = build_env([provide!(ConfigLoaderLive)]).expect("build");
    assert_eq!(env.get::<Cap<AppConfig>>(), "loaded-from-effect");
  }
}
