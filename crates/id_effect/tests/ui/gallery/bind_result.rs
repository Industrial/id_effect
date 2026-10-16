// Gallery: `~` on a plain `Result`; `~` binds effects only. Fix: use `?` on a `Result`.
use id_effect::{Effect, effect};

pub struct ParseError;

fn parse(s: &str) -> Result<u32, ParseError> {
  s.parse().map_err(|_| ParseError)
}

fn handler() -> Effect<u32, ParseError> {
  effect!({ ~parse("42") })
}

fn main() {
  let _ = handler();
}
