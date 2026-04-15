//! Ex 030 — `service_def!` pairs a key with a service alias.
use id_effect::{service, service_def};

service_def!(struct DbKey as DbSvc => u32);

fn main() {
  let s: DbSvc = service::<DbKey, _>(7);
  assert_eq!(s.value, 7);
  println!("030_service_def ok");
}
