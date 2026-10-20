// E-03: thread-local storage (`#[thread_local]`, what `thread_local!` builds on).
#![feature(thread_local)]
#![allow(dead_code)]
#![allow(no_static_service_globals)]

use std::cell::Cell;

#[thread_local]
static REQUEST_ID: Cell<u64> = Cell::new(0);

thread_local! {
  static TENANT: Cell<u64> = const { Cell::new(0) };
}

fn main() {}
