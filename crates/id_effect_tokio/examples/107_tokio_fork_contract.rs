//! Ex 107 — `TokioRuntime::fork` runs the effect as a fiber on the Tokio runtime; joining the
//! handle returns its `Exit`, and interrupting it ends it with `Cause::Interrupt`.
//!
//! Run: `cargo run -p id_effect_tokio --example 107_tokio_fork_contract`

use std::time::Duration;

use id_effect::next::{HNil, sleep, succeed};
use id_effect::{Cause, Exit};
use id_effect_tokio::TokioRuntime;

fn main() {
  let rt = TokioRuntime::new_current_thread().expect("tokio runtime should build");

  let forked = rt.fork(succeed::<u8, (), HNil>(5));
  let out = rt.block_on(forked.join_future());
  assert!(matches!(out, Exit::Success(5)));

  let sleeper = rt.fork(sleep::<(), HNil>(Duration::from_secs(60)));
  let id = sleeper.id();
  sleeper.interrupt_now();
  let out = rt.block_on(sleeper.join_future());
  assert!(matches!(out, Exit::Failure(Cause::Interrupt(by)) if by == id));

  println!("107_tokio_fork_contract ok");
}
