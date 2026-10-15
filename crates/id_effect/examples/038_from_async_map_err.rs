//! Ex 038 — Map the error of an async effect with `map_err`.
use id_effect::{Cause, Exit};
use id_effect::{Effect, from_async};

fn main() {
  let eff: Effect<u8, usize> =
    from_async(|| async move { Err::<u8, &'static str>("boom") }).map_err(|e| e.len());
  assert_eq!(eff.block_on(), Exit::Failure(Cause::Fail(4)));
  println!("038_from_async_map_err ok");
}
