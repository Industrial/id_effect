// An `effect_next!` body over a typed `R` is `Send` (exit item 3, macro form): it holds only
// `Send` values across its suspension points, and the resulting `Effect` crosses threads.
use std::future::Future;

use id_effect::next::{Effect, caps_next, effect_next, hlist};
use id_effect::{Exit, Never};

fn assert_send<T: Send>(_: &T) {}

pub struct Sessions;

impl Sessions {
  pub fn get(&self, sid: &str) -> impl Future<Output = Option<String>> + Send + 'static {
    std::future::ready(Some(format!("user-of-{sid}")))
  }
}

pub struct Counter(pub usize);

fn handler() -> Effect<usize, Never, caps_next!(Sessions, Counter)> {
  effect_next!({
    let owned = String::from("send");
    let user = ~Sessions.get("demo").await;
    owned.len() + user.map_or(0, |u| u.len()) + ~Counter.0
  })
}

fn main() {
  let eff = handler();
  assert_send(&eff);
  let exit = std::thread::spawn(move || eff.provide_env(hlist![Sessions, Counter(1)]).block_on())
    .join()
    .unwrap_or_else(|_| panic!("the effect panicked"));
  assert!(matches!(exit, Exit::Success(17)));
}
