//! Ex 013 — `fail(domain).map_err(Into::into)` when `E: From<SmallErr>`; `effect!` widens a
//! bound effect's error through `From` on its own.
use id_effect::{Cause, Exit};
use id_effect::{Effect, effect, fail};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AppErr {
  Msg(&'static str),
}
impl From<&'static str> for AppErr {
  fn from(s: &'static str) -> Self {
    AppErr::Msg(s)
  }
}

fn step() -> Effect<(), &'static str> {
  fail("boom")
}

fn main() {
  let program: Effect<(), AppErr> = step().map_err(Into::into);
  assert_eq!(
    program.block_on(),
    Exit::Failure(Cause::Fail(AppErr::Msg("boom")))
  );
  let widened: Effect<(), AppErr> = effect!({ ~step() });
  assert_eq!(
    widened.block_on(),
    Exit::Failure(Cause::Fail(AppErr::Msg("boom")))
  );
  println!("013_fail_into ok");
}
