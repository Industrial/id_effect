use id_effect::{Effect, Needs};

struct MissingCapKey;

fn bad() -> Effect<(), (), ()> {
  Effect::new(|r: &mut ()| {
    let _ = Needs::<MissingCapKey>::need(r);
    Ok(())
  })
}

fn main() {
  let _ = bad();
}
