//! Ex 008 — `effect!` closure receives `&mut R` (the environment).
use id_effect::{Cons, Context, Get, Nil, Tagged, ctx, effect, run_blocking, service_key, succeed};

service_key!(struct CounterKey);

type Env = Context<Cons<Tagged<CounterKey, i32>, Nil>>;

fn main() {
  let program = effect!(|r: &mut Env| {
    let n = ~succeed(*Get::<CounterKey>::get(r));
    n + 1
  });
  let env = ctx!(CounterKey => 41_i32);
  assert_eq!(run_blocking(program, env), Ok::<i32, ()>(42));
  println!("008_effect_macro_env ok");
}
