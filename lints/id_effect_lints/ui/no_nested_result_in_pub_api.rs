// F-01: `Result<Result<_, _>, _>` in a public API.
#![allow(dead_code)]
#![allow(no_result_return_in_pub_fn)]

pub type Nested = Result<Result<u8, ()>, ()>;

pub fn nested() -> Result<Result<u8, ()>, ()> {
  Ok(Ok(1))
}

pub type Flat = Result<u8, ()>;

fn main() {}
