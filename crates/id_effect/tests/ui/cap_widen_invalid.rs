use id_effect::{CapList, CapWiden, Env};

#[::id_effect::capability(u32)]
struct Db;
#[::id_effect::capability(u32)]
struct Log;

fn narrow(_: CapList<(DbKey,)>) {}

fn main() {
  let mut env = Env::new();
  env.insert::<DbKey>(1u32);
  let wide = CapList::<(DbKey,)>::from_env(env);
  narrow(wide); // should use widen for subset; direct assign of wrong arity
  let _: CapList<(DbKey, LogKey)> = wide;
}
