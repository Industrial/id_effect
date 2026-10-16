// Gallery: `~sub` whose error the body's error type cannot absorb (`~` widens with `From`).
// Fix: `impl From<DbError> for AppError`, or `~load().map_err(AppError::Db)`.
use id_effect::{Effect, effect, fail};

pub struct DbError;

pub enum AppError {
  NotFound,
}

fn load() -> Effect<u32, DbError> {
  fail(DbError)
}

fn handler() -> Effect<u32, AppError> {
  effect!({
    let n = ~load();
    if n == 0 {
      return Err(AppError::NotFound);
    }
    n
  })
}

fn main() {
  let _ = handler();
}
