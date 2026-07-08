#![allow(dead_code, clippy::new_ret_no_self, clippy::type_complexity)]

//! Integration: automatic cap_into_bind + implicit `|r|` + `~Service`.

use id_effect::{Effect, caps, effect, provide, run_with};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Database(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Logger(pub u32);

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Database)]
struct DatabaseLive;

impl DatabaseLive {
  fn new() -> Database {
    Database(10)
  }
}

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Logger)]
struct LoggerLive;

impl LoggerLive {
  fn new() -> Logger {
    Logger(99)
  }
}

fn query(_id: u64) -> Effect<u32, (), caps!(Database)> {
  effect!(|r| {
    let db = ~Database;
    db.0
  })
}

fn log_event(_msg: &str) -> Effect<(), (), caps!(Logger)> {
  effect!(|r| {
    let _log = ~Logger;
  })
}

#[test]
fn bind_narrow_effects_in_wider_caps() {
  let program: Effect<u32, (), caps!(Database, Logger)> = effect!(|r| {
    ~log_event("start");
    let n = ~query(1);
    n
  });

  let out = run_with([provide!(DatabaseLive), provide!(LoggerLive)], program).unwrap();
  assert_eq!(out, 10);
}
