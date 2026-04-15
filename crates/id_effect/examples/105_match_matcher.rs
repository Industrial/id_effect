//! Ex 105 — `Matcher` + `HasTag` route values by string discriminant.
use id_effect::{HasTag, Matcher};

#[derive(Debug, Clone)]
struct Msg(&'static str, i32);

impl HasTag for Msg {
  fn tag(&self) -> &str {
    self.0
  }
}

fn main() {
  let m = Matcher::new()
    .tag("a", |m: Msg| m.1 * 10)
    .tag("b", |m: Msg| m.1 + 1)
    .or_else(|m: Msg| m.1);
  let f = m.exhaustive();
  assert_eq!(f(Msg("a", 2)), 20);
  assert_eq!(f(Msg("b", 2)), 3);
  assert_eq!(f(Msg("c", 7)), 7);
  println!("105_match_matcher ok");
}
