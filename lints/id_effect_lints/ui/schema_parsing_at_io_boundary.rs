// K-01: `serde_json::from_*(…).unwrap()` instead of a `Schema` at the boundary.
#![allow(dead_code)]
#![allow(no_result_return_in_pub_fn)]

mod serde_json {
  pub fn from_str(_s: &str) -> Result<u8, ()> {
    Ok(0)
  }
}

fn parse(body: &str) -> u8 {
  serde_json::from_str(body).unwrap()
}

fn parse_checked(body: &str) -> Result<u8, ()> {
  serde_json::from_str(body)
}

fn main() {}
