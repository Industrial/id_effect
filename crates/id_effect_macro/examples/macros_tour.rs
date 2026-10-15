//! Tour of the kernel-neutral declarative macros: `pipe!` and `err!`.
//!
//! Run: `cargo run -p id_effect_macro --example macros_tour`
//!
//! The environment of an effect is named with `id_effect::caps!` (a proc macro); capabilities are
//! provided with `.provide(..)` / `.provide_env(..)` or `Layer`s.

use id_effect::Or;
use id_effect_macro::{err, pipe};

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

fn main() {
  let shouted = pipe!("  id_effect ", str::trim, str::to_uppercase);
  println!("pipe!: {shouted}");

  println!(
    "err!: {:?} / {:?}",
    parse_percent("42"),
    parse_percent("420")
  );
}
