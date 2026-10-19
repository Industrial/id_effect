//! Code that `id_effect_lints` warns about, one finding per item (book appendix D). It builds on
//! stable with no dependencies: `Effect` and the SQL traits are stand-ins with id_effect's
//! names, which is all the lints look at.
//!
//! ```bash
//! cd lints/id_effect_lints/examples/demo
//! cargo dylint --all                                          # Dylint (pinned nightly)
//! ast-grep scan -c ../../sgconfig.yml .                       # ast-grep (no nightly)
//! ```
//!
//! CI (`.github/workflows/lint.yml`) runs both and fails unless each finding below is reported.
#![allow(dead_code)]
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::SystemTime;

pub struct Effect<A, E, R>(PhantomData<(A, E, R)>);

pub struct SqlValue;
pub struct SqlRow;
pub struct SqlError;

pub trait SqlClient {
  fn query(&self, sql: &str, params: &[SqlValue]) -> Effect<Vec<SqlRow>, SqlError, ()>;
}

// no_result_return_in_pub_fn / id_effect-no-result-return-pub-fn
pub fn parse_port(s: &str) -> Result<u16, std::num::ParseIntError> {
  s.parse()
}

// no_unwrap_inside_effect / id_effect-no-unwrap-in-effect
fn load_port(s: &str) -> Effect<u16, (), ()> {
  let _port: u16 = s.parse().unwrap();
  Effect(PhantomData)
}

// no_system_time_in_domain / id_effect-no-system-time-in-domain
fn stamp() -> SystemTime {
  SystemTime::now()
}

// no_rc_refcell_in_effect_code / id_effect-no-rc-refcell
struct Cache {
  entries: Rc<RefCell<Vec<u8>>>,
}

// no_thread_spawn / id_effect-no-thread-spawn
fn detach() {
  std::thread::spawn(|| {});
}

// raw_sql_non_literal (Dylint only: it needs the argument's type)
fn find(c: &dyn SqlClient, table: &str) -> Effect<Vec<SqlRow>, SqlError, ()> {
  let sql = format!("SELECT id FROM {table}");
  c.query(&sql, &[])
}
