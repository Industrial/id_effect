// I-01: `std::thread::sleep` in a test (H-01 outside tests). Compiled with `--test`.
#![allow(dead_code)]
use std::time::Duration;

#[test]
fn waits_on_the_real_clock() {
  std::thread::sleep(Duration::from_millis(1));
}

// A helper in a `#[cfg(test)]` module is test code too.
#[cfg(test)]
mod helpers {
  pub fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(1));
  }
}
