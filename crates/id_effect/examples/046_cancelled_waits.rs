//! Ex 046 — `cancelled()` completes after `cancel`.
use id_effect::CancellationToken;

fn main() {
  let t = CancellationToken::new();
  let w = t.clone();
  std::thread::spawn(move || {
    std::thread::sleep(std::time::Duration::from_millis(5));
    w.cancel();
  });
  assert_eq!(pollster::block_on(t.cancelled().run(&mut ())), Ok(()));
  println!("046_cancelled_waits ok");
}
