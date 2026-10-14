// (4) A `!Send` value held across a suspension point in a (Send-by-default) `effect!` body.
// `effect_local!` accepts the same body.
use std::rc::Rc;

use kernel_a::{Effect, Never, caps, effect};
use spike_corpus::Sessions;

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
