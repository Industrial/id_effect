// Gallery: `~sub` with the right arm of an `Or<L, R>` error; `From` only lifts into `Or::Left`.
// Fix: `~send().map_err(Or::Right)`.
use id_effect::Or;
use id_effect::{Effect, effect, fail};

pub struct DbError;

pub struct MailError;

fn load() -> Effect<u32, DbError> {
  fail(DbError)
}

fn send() -> Effect<(), MailError> {
  fail(MailError)
}

fn handler() -> Effect<u32, Or<DbError, MailError>> {
  effect!({
    let n = ~load();
    ~send();
    n
  })
}

fn main() {
  let _ = handler();
}
