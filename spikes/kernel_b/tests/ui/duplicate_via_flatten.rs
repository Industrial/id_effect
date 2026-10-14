// (2b) A duplicate that arrives through `#[env(flatten)]`: the derive cannot see the base's
// fields, so this surfaces as rustc's E0119 (design §2.5: readability "fair").
use kernel_b::Env;
use spike_corpus::{ConfigSource, Logger};

#[derive(Clone, Env)]
struct BaseEnv {
  config: ConfigSource,
  logger: Logger,
}

#[derive(Clone, Env)]
struct AppEnv {
  #[env(flatten)]
  base: BaseEnv,
  logger: Logger,
}

fn main() {}
