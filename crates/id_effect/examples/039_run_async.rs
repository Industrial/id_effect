//! Ex 039 — `run_async` polls an effect to completion.
use id_effect::{run_async, succeed};

fn main() {
  assert_eq!(
    pollster::block_on(run_async(succeed::<u8, (), ()>(9_u8), ())),
    Ok(9)
  );
  println!("039_run_async ok");
}
