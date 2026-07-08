use id_effect::{Effect, Needs};

struct MissingCap;

fn bad() -> Effect<(), (), ()> {
  Effect::new(|r: &mut ()| {
    let _ = Needs::<MissingCap>::need(r);
    Ok(())
  })
}

fn main() {
  let _ = bad();
}
