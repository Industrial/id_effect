// (4) A `!Send` local held across a bind in a (Send-by-default) `effect!` body.
// The fix is `effect_local!` (a `LocalEffect`).
use std::rc::Rc;

use kernel_b::{Effect, Has, Never, effect};
use spike_corpus::Sessions;

fn cached_user<R: Has<Sessions>>() -> Effect<String, Never, R> {
  effect!(|r| {
    let cache = Rc::new(String::from("cache"));
    let user = ~Sessions.get("demo").await;
    format!("{cache}:{user:?}")
  })
}

fn main() {
  let _ = cached_user().provide(Sessions::default()).block_on();
}
