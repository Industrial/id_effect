// D-03: `tokio::spawn`.
#![allow(dead_code)]

mod tokio {
  pub fn spawn<F>(_f: F) {}
}

fn detach() {
  tokio::spawn(async {});
}

fn main() {}
