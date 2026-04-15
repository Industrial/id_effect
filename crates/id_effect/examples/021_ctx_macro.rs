//! Ex 021 — `ctx!` builds a `Context` from key/value pairs.
use id_effect::{Cons, Context, Get, Nil, Tagged, ctx, run_blocking, service_key, succeed};

service_key!(struct PortKey);

type Env = Context<Cons<Tagged<PortKey, u16>, Nil>>;

fn main() {
  let env: Env = ctx!(PortKey => 8080_u16);
  assert_eq!(*Get::<PortKey>::get(&env), 8080);
  assert_eq!(
    run_blocking(succeed::<(), (), Env>(()), env),
    Ok::<(), ()>(())
  );
  println!("021_ctx_macro ok");
}
