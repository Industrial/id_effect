use id_effect::{Effect, Env, Needs};
struct Counter;

fn bad() -> Effect<(), (), Env> {
  Effect::new(|env: &mut Env| {
    let _ = id_effect::require!(env, Counter);
    Ok(())
  })
}

fn main() {
  let _ = bad();
}
