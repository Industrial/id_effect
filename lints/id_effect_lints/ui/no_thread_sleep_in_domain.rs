// H-01: `std::thread::sleep` in domain code.
#![allow(dead_code)]
use std::time::Duration;

fn wait() {
  std::thread::sleep(Duration::from_millis(10));
}

fn main() {}
