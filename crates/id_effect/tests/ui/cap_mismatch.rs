use id_effect::{Effect, Needs};

#[::id_effect::capability(u32)]
struct Counter;

fn bad() -> Effect<(), (), ()> {
  Effect::new(|r: &mut ()| {
    let _ = Needs::<CounterKey>::need(r);
    Ok(())
  })
}

fn main() {
  let _ = bad();
}
