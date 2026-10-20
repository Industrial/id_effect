// D-04: `std::thread::spawn`.
#![allow(dead_code)]

fn detach() {
  let _ = std::thread::spawn(|| {});
}

fn main() {}
