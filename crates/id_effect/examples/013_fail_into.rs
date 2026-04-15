//! Ex 013 — `fail(domain).map_error(Into::into)` when `E: From<SmallErr>`.
use id_effect::{Effect, fail, run_blocking};

#[derive(Debug, Clone, PartialEq, Eq)]
enum AppErr {
  Msg(&'static str),
}
impl From<&'static str> for AppErr {
  fn from(s: &'static str) -> Self {
    AppErr::Msg(s)
  }
}

fn main() {
  let program: Effect<(), AppErr, ()> = fail("boom").map_error(Into::into);
  assert_eq!(run_blocking(program, ()), Err(AppErr::Msg("boom")));
  println!("013_fail_into ok");
}
