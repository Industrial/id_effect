// A-01: `async fn` outside tests.
#![allow(dead_code)]

async fn bad_fetch(url: &str) -> String {
  url.to_string()
}

fn fine(url: &str) -> String {
  url.to_string()
}

fn main() {}
