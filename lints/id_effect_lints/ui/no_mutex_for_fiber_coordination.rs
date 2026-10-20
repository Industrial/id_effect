// G-03: `Arc<Mutex<…>>` / `Arc<RwLock<…>>` for shared state.
#![allow(dead_code)]
use std::sync::{Arc, Mutex, RwLock};

struct Shared {
  counter: Arc<Mutex<u64>>,
  config: Arc<RwLock<String>>,
  plain: Arc<String>,
}

fn main() {}
