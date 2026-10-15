// Spike A's `duplicate_capability_layer` (E0283 there): two layers that both need `ConfigSource`,
// merged side by side. The merged input holds `ConfigSource` twice and is split by position, so
// this compiles with no annotation and both layers see the same `Arc`.
use std::sync::Arc;

use id_effect::Exit;
use id_effect::next::{Layer, caps_next, effect_next, hlist};

#[derive(Debug)]
pub struct ConfigError;

#[derive(Debug)]
pub enum AppError {
  Config(ConfigError),
}

impl From<ConfigError> for AppError {
  fn from(e: ConfigError) -> Self {
    Self::Config(e)
  }
}

pub struct ConfigSource(&'static str);

impl ConfigSource {
  pub fn get_string(&self, key: &str) -> Result<String, ConfigError> {
    Ok(format!("{}/{key}", self.0))
  }
}

pub struct Db(String, Arc<ConfigSource>);

pub struct Mailer(String, Arc<ConfigSource>);

fn db_layer() -> Layer<caps_next!(Db), AppError, caps_next!(ConfigSource)> {
  Layer::from_effect(effect_next!(|r: &caps_next!(ConfigSource)| {
    let url = ~ConfigSource.get_string("db_url")?;
    hlist![Db(url, Arc::clone(&r.0))]
  }))
}

fn mail_layer() -> Layer<caps_next!(Mailer), AppError, caps_next!(ConfigSource)> {
  Layer::from_effect(effect_next!(|r: &caps_next!(ConfigSource)| {
    let relay = ~ConfigSource.get_string("smtp")?;
    hlist![Mailer(relay, Arc::clone(&r.0))]
  }))
}

fn main() {
  let infra = db_layer().merge(mail_layer());
  let built = infra
    .build()
    .provide_env(hlist![ConfigSource("mem://app")])
    .block_on();
  let Exit::Success(env) = built else {
    panic!("the merged layer failed to build");
  };
  let (db, mailer) = (&env.0, &env.1.0);
  assert_eq!(
    (db.0.as_str(), mailer.0.as_str()),
    ("mem://app/db_url", "mem://app/smtp")
  );
  assert!(Arc::ptr_eq(&db.1, &mailer.1));
}
