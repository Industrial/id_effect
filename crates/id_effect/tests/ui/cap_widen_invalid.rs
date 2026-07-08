use id_effect::{CapList, CapWiden, Env};
struct Db;
struct Log;

fn narrow(_: CapList<(Db,)>) {}

fn main() {
  let mut env = Env::new();
  env.insert::<Cap<Db>>(1u32);
  let wide = CapList::<(Db,)>::from_env(env);
  narrow(wide); // should use widen for subset; direct assign of wrong arity
  let _: CapList<(Db, Log)> = wide;
}
