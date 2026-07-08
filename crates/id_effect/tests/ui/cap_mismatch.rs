use id_effect::{Effect, Needs};
struct Counter;

fn bad() -> Effect<(), (), ()> {
  Effect::new(|r: &mut ()| {
    let _ = Needs::<Counter>::need(r);
    Ok(())
  })
}

fn main() {
  let _ = bad();
}
