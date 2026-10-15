//! Ex 089 — `Effect::ensuring` runs a finalizer after the body, on success and on failure.
use std::sync::{Arc, Mutex};

use id_effect::Never;
use id_effect::next::{Effect, fail, succeed, sync};

fn bump_fin(calls: Arc<Mutex<usize>>) -> Effect<(), Never> {
  sync(move || *calls.lock().expect("mutex") += 1)
}

fn main() {
  let calls = Arc::new(Mutex::new(0usize));

  let ok: Effect<u8, &'static str> = succeed(8);
  let exit = ok.ensuring(bump_fin(Arc::clone(&calls))).block_on();
  assert_eq!(exit.into_result().ok(), Some(8));
  assert_eq!(*calls.lock().expect("mutex"), 1);

  let err: Effect<u8, &'static str> = fail("err");
  assert!(
    err
      .ensuring(bump_fin(Arc::clone(&calls)))
      .block_on()
      .into_result()
      .is_err()
  );
  assert_eq!(*calls.lock().expect("mutex"), 2);

  println!("089_ensuring ok");
}
