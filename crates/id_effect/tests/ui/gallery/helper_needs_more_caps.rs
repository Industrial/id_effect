// Gallery: `~helper()` where the helper needs a capability the caller does not have.
// Fix: add `Db` to the caller's `caps!(..)` (or provide it to the helper first).
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

fn report() -> Effect<usize, DbError, caps!(Logger)> {
  effect!({
    let n = ~count_users();
    ~Logger.info(format!("{n} users"));
    n
  })
}

fn main() {
  let _ = report();
}
