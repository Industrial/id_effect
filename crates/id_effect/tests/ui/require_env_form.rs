use id_effect::{Effect, Env, Needs};

#[::id_effect::capability(u32)]
struct Counter;

fn bad() -> Effect<(), (), Env> {
  Effect::new(|env: &mut Env| {
    let _ = id_effect::require!(env, CounterKey);
    Ok(())
  })
}

fn main() {
  let _ = bad();
}
