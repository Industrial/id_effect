//! Ex 090 — `acquire_release` runs cleanup after the body completes.
use id_effect::{acquire_release, run_blocking, succeed};

fn main() {
  let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
  let log2 = std::sync::Arc::clone(&log);
  let eff = acquire_release(succeed::<u8, &'static str, ()>(9), move |n| {
    let log2 = std::sync::Arc::clone(&log2);
    id_effect::Effect::new(move |_env: &mut ()| {
      log2.lock().unwrap().push(n);
      Ok::<(), ()>(())
    })
  });
  assert_eq!(run_blocking(eff, ()), Ok(9));
  assert_eq!(*log.lock().unwrap(), vec![9]);
  println!("090_acquire_release ok");
}
