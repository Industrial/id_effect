// A-02: a public fn returning `Result`.
#![allow(dead_code)]

pub fn parse(s: &str) -> Result<u32, std::num::ParseIntError> {
  s.parse()
}

// Private functions and `main` may return `Result`.
fn parse_private(s: &str) -> Result<u32, std::num::ParseIntError> {
  s.parse()
}

fn main() -> Result<(), ()> {
  Ok(())
}
