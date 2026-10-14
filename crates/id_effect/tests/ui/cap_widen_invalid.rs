//! `CapWiden` may only drop capabilities (subset); widening must never add one.
use id_effect::{Cap, CapList, CapWiden, Env};

#[derive(Clone)]
struct Db;
#[derive(Clone)]
struct Log;

fn main() {
  let narrow = CapList::<(Cap<Db>,)>::new(Env::new());
  let _wide: CapList<(Cap<Db>, Cap<Log>)> = narrow.widen();
}
