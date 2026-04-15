//! Ex 038 — Map errors on async effects with `map_error`.
use id_effect::{Effect, box_future, run_async};

fn main() {
  let eff =
    Effect::new_async(|_r: &mut ()| box_future(async move { Err::<u8, &'static str>("boom") }))
      .map_error(|e| e.len());
  assert_eq!(pollster::block_on(run_async(eff, ())), Err(4));
  println!("038_from_async_map_err ok");
}
