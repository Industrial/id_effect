// Gallery: a `!Send` value (`Rc`) held across a suspension point in an `effect!` body (which is
// `Send` by default). Fix: use `Arc`, drop the value before the `~`, or write `effect_local!`.
use std::future::Future;
use std::rc::Rc;

use id_effect::Never;
use id_effect::{Effect, caps, effect};

pub struct Sessions;

impl Sessions {
  pub fn get(&self, _sid: &str) -> impl Future<Output = Option<String>> + Send + 'static {
    std::future::ready(None)
  }
}

fn handler() -> Effect<usize, Never, caps!(Sessions)> {
  effect!({
    let local = Rc::new(String::from("not send"));
    let user = ~Sessions.get("demo").await;
    local.len() + user.map_or(0, |u| u.len())
  })
}

fn main() {
  let _ = handler();
}
