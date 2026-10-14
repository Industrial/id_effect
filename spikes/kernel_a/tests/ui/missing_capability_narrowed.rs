// (1b) A `~` bind of a helper whose `R` needs a capability the caller lacks (narrowing fails).
use kernel_a::{Effect, caps, effect};
use spike_corpus::{Db, DbError, Logger};

fn count_users() -> Effect<usize, DbError, caps!(Db)> {
  effect!({ ~Db.count("users").await? })
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
