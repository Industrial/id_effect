//! Ex 102 — A `Semaphore` gates concurrent work; a `Permit` is released on drop.
use id_effect::{Effect, Semaphore};

fn main() {
  let sem = Semaphore::new(1);
  let first: Effect<bool, ()> = sem.try_acquire().map(|p| p.is_some());
  assert_eq!(first.block_on().into_result().ok(), Some(true));
  let guarded: Effect<usize, ()> = sem.with_permit(sem.available());
  assert_eq!(guarded.block_on().into_result().ok(), Some(0));
  println!("102_semaphore ok");
}
