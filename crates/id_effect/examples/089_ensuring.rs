//! Ex 089 — `Effect::ensuring` runs a finalizer [`Effect`] after the body completes, success or failure.
use id_effect::{Effect, Never, fail, run_blocking, succeed};
use std::sync::{Arc, Mutex};

fn bump_fin(calls: Arc<Mutex<usize>>) -> Effect<(), Never, ()> {
  Effect::new(move |_env: &mut ()| {
    *calls.lock().expect("mutex") += 1;
    Ok::<(), Never>(())
  })
}

fn main() {
  let calls = Arc::new(Mutex::new(0usize));

  assert_eq!(
    run_blocking(
      succeed::<u8, &'static str, ()>(8).ensuring(bump_fin(Arc::clone(&calls))),
      (),
    ),
    Ok(8)
  );
  assert_eq!(*calls.lock().expect("mutex"), 1);

  assert!(
    run_blocking(
      fail::<u8, &'static str, ()>("err").ensuring(bump_fin(Arc::clone(&calls))),
      (),
    )
    .is_err()
  );
  assert_eq!(*calls.lock().expect("mutex"), 2);

  println!("089_ensuring ok");
}
