// H-02: reading the wall clock directly.
#![allow(dead_code)]
use std::time::SystemTime;

fn stamp() -> SystemTime {
  SystemTime::now()
}

fn stamp_qualified() -> SystemTime {
  std::time::SystemTime::now()
}

fn main() {}
