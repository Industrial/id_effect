// (4) A `!Send` value held across a suspension point in a (Send-by-default) `effect_next!` body.
// `effect_local_next!` accepts the same body.
use std::future::Future;
use std::rc::Rc;

use id_effect::Never;
use id_effect::next::{Effect, caps_next, effect_next};

pub struct Sessions;

impl Sessions {
  pub fn get(&self, _sid: &str) -> impl Future<Output = Option<String>> + Send + 'static {
    std::future::ready(None)
  }
}

fn handler() -> Effect<usize, Never, caps_next!(Sessions)> {
  effect_next!({
    let local = Rc::new(String::from("not send"));
    let user = ~Sessions.get("demo").await;
    local.len() + user.map_or(0, |u| u.len())
  })
}

fn main() {
  let _ = handler();
}
