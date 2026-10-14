//! Tour of the declarative macros: `pipe!`, `err!`, `caps!`, `mock_capability!`, `providers!`.
//!
//! Run: `cargo run -p id_effect_macro --example macros_tour`

use id_effect::{Cap, Or, build_env};
use id_effect_macro::{caps, err, mock_capability, pipe, providers};

#[derive(Clone, Debug)]
struct Greeting(&'static str);

mock_capability!(GreetingMock, Greeting, "greeting/mock", || Greeting(
  "hello"
));

#[derive(Debug)]
struct ParseErr;
#[derive(Debug)]
struct RangeErr;

/// Fails with one of two error types, summed by `err!`.
fn parse_percent(s: &str) -> Result<u8, err!(ParseErr | RangeErr)> {
  let n: u32 = s.trim().parse().map_err(|_| Or::Left(ParseErr))?;
  u8::try_from(n)
    .ok()
    .filter(|n| *n <= 100)
    .ok_or(Or::Right(RangeErr))
}

/// Only type-checks because `caps!(Greeting)` names the capability list this fn needs.
fn greet(env: &caps!(Greeting)) -> &'static str {
  env.env().get::<Cap<Greeting>>().0
}

fn main() {
  let shouted = pipe!("  id_effect ", str::trim, str::to_uppercase);
  println!("pipe!: {shouted}");

  println!(
    "err!: {:?} / {:?}",
    parse_percent("42"),
    parse_percent("420")
  );

  let env = build_env(providers!(dev: [GreetingMock])).expect("providers build");
  let typed = id_effect::CapList::new(env);
  println!("caps!: {}", greet(&typed));
}
