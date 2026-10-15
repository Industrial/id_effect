// The H17 pattern: `fail/generic_helper_bind.rs` with `.same_env()`. The helper is generic in
// `R`; `.same_env()` fixes it to the caller's environment, so neither caller needs a turbofish.
use std::sync::{Arc, Mutex};

use id_effect::Exit;
use id_effect::{Effect, HList, Selector, caps, effect, hlist};

#[derive(Debug)]
pub struct AppError;

pub struct Db;

pub struct Mailer;

#[derive(Clone, Default)]
pub struct Logger(Arc<Mutex<Vec<String>>>);

impl Logger {
  pub fn info(&self, line: String) {
    self.0.lock().unwrap_or_else(|p| p.into_inner()).push(line);
  }
}

fn audit<R, I>(msg: String) -> Effect<(), AppError, R>
where
  R: HList + Selector<Logger, I>,
{
  effect!({ ~Logger.info(msg) })
}

fn signup() -> Effect<(), AppError, caps!(Logger, Db)> {
  effect!({ ~audit("signup".to_string()).same_env() })
}

fn notify() -> Effect<(), AppError, caps!(Mailer, Logger)> {
  effect!({ ~audit("notify".to_string()).same_env() })
}

fn main() {
  let logger = Logger::default();
  let a = signup().provide_env(hlist![logger.clone(), Db]).block_on();
  let b = notify()
    .provide_env(hlist![Mailer, logger.clone()])
    .block_on();
  assert!(matches!((a, b), (Exit::Success(()), Exit::Success(()))));
  assert_eq!(
    *logger.0.lock().unwrap_or_else(|p| p.into_inner()),
    ["signup", "notify"]
  );
}
