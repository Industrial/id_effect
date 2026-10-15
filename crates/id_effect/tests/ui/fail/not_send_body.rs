// (4) A `!Send` value held across a suspension point in a (Send-by-default) `effect!` body.
// `effect_local!` accepts the same body.
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
