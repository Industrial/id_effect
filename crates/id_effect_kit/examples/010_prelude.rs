//! Ex 010 — one `use id_effect_kit::prelude::*;` for a whole program.
//!
//! The core part (effects, typed errors, retry, secrets, `run_main`) runs with the default
//! features; with `--features web` it also assembles the observability routes.
//!
//! Run: `cargo run -p id_effect_kit --example 010_prelude` (or `--features full`)

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use id_effect_kit::prelude::*;

#[derive(Debug)]
struct Unavailable;

#[derive(Debug)]
struct Rejected(&'static str);

/// A call that fails twice, then answers — retried per a `Schedule`.
fn flaky(calls: &Arc<AtomicU32>) -> Effect<u32, Unavailable> {
  let calls = Arc::clone(calls);
  Effect::retry(
    move || {
      let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
      if n < 3 { fail(Unavailable) } else { succeed(n) }
    },
    Schedule::spaced(Duration::from_millis(1)).intersect(Schedule::recurs(5)),
  )
}

/// Two error types in one channel: `err!(A | B)` is `Or<A, B>`.
fn validate(token: &Secret<String>) -> Effect<usize, err!(Unavailable | Rejected)> {
  let len = token.expose().len();
  if len < 4 {
    fail(Or::Right(Rejected("token too short")))
  } else {
    succeed(len)
  }
}

fn program(token: Secret<String>) -> Effect<(), err!(Unavailable | Rejected)> {
  let calls = Arc::new(AtomicU32::new(0));
  flaky(&calls)
    .map_err(Or::Left)
    .flat_map(move |attempts| {
      validate(&token).map(move |len| {
        println!("upstream answered after {attempts} attempts; token {token:?} has {len} chars");
      })
    })
    .flat_map(|()| sleep(Duration::from_millis(1)))
}

#[cfg(feature = "web")]
fn routes() {
  let router = observability_routes(Arc::new(|| true)).merge(metrics_router());
  let _ = router;
  println!("web: /health, /ready and /metrics assembled");
}

#[cfg(not(feature = "web"))]
fn routes() {
  println!("web: off (run with --features web)");
}

fn main() -> ExitCode {
  routes();
  match validate(&Secret::new("abc".into())).block_on() {
    Exit::Failure(Cause::Fail(Or::Right(Rejected(reason)))) => println!("rejected: {reason}"),
    other => panic!("expected a rejection, got {other:?}"),
  }
  run_main(
    program(Secret::new("hunter22".into())),
    hlist![],
    RunMainConfig::minimal(),
  )
}
