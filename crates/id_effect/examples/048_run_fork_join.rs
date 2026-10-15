//! Ex 048 — `fork` runs an effect on a child fiber; `join` awaits its outcome.
use id_effect::Exit;
use id_effect::{Effect, succeed};

fn main() {
  let child: Effect<u8, ()> = succeed(5);
  let program: Effect<u8, ()> = child.fork().widen().flat_map(|h| h.join());
  assert_eq!(program.block_on(), Exit::succeed(5));
  println!("048_run_fork_join ok");
}
