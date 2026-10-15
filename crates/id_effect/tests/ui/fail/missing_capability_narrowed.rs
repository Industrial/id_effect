// (1b) A `~` bind of a helper whose `R` needs a capability the caller lacks (narrowing fails).
use id_effect::{Effect, caps, effect};

pub struct DbError;

pub struct Db;

impl Db {
  pub fn count(&self, _table: &str) -> Result<usize, DbError> {
    Ok(0)
  }
}

pub struct Logger;

impl Logger {
  pub fn info(&self, _line: String) {}
}

fn count_users() -> Effect<usize, DbError, caps!(Db)> {
  effect!({ ~Db.count("users")? })
}

fn handler() -> Effect<usize, DbError, caps!(Logger)> {
  effect!({
    let n = ~count_users();
    ~Logger.info(format!("{n}"));
    n
  })
}

fn main() {
  let _ = handler();
}
