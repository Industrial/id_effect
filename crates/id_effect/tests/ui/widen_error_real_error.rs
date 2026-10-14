//! `widen_error` only lifts `Never` / `Infallible` / `()` errors; a real error must be converted.
use id_effect::{Effect, fail};

#[derive(Debug)]
struct DbError;

#[derive(Debug)]
struct AppError;

fn main() {
  let query: Effect<u32, DbError, ()> = fail(DbError);
  let _: Effect<u32, AppError, ()> = query.widen_error();
}
