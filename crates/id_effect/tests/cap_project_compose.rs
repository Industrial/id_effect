#![allow(dead_code, clippy::new_ret_no_self)]

//! Integration: automatic cap_into_bind + implicit `|r|` + `~Key`.

use id_effect::{Effect, caps, effect, provide, run_with};

#[::id_effect::capability(u32)]
struct Database;

#[::id_effect::capability(u32)]
struct Logger;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(DatabaseKey)]
struct DatabaseLive;

impl DatabaseLive {
  fn new() -> u32 {
    10
  }
}

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(LoggerKey)]
struct LoggerLive;

impl LoggerLive {
  fn new() -> u32 {
    99
  }
}

fn query(_id: u64) -> Effect<u32, (), caps!(DatabaseKey)> {
  effect!(|r| {
    let db = ~DatabaseKey;
    *db
  })
}

fn log_event(_msg: &str) -> Effect<(), (), caps!(LoggerKey)> {
  effect!(|r| {
    let _log = ~LoggerKey;
  })
}

#[test]
fn bind_narrow_effects_in_wider_caps() {
  let program: Effect<u32, (), caps!(DatabaseKey, LoggerKey)> = effect!(|r| {
    ~log_event("start");
    let n = ~query(1);
    n
  });

  let out = run_with([provide!(DatabaseLive), provide!(LoggerLive)], program).unwrap();
  assert_eq!(out, 10);
}
