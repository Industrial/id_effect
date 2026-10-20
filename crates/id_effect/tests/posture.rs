//! ADR 0013 R10: the posture registry records every `dangerous_*` / `insecure_*` opt-out once.
//!
//! The registry is process-wide. nextest runs each test in its own process; under plain
//! `cargo test` the tests share one, so each test uses its own ids and the empty check only
//! rejects ids this file does not record.

use id_effect::Exit;
use id_effect::observability::posture::{dangerous_opt_outs, record_dangerous};
use id_effect::{Effect, succeed};

const PREFIX: &str = "tests::posture::";

#[test]
fn posture_is_empty_by_default() {
  let eff: Effect<u32, ()> = succeed(1).map(|n| n + 1).with_span("posture");
  assert!(matches!(eff.block_on(), Exit::Success(2)));
  let found = dangerous_opt_outs();
  assert!(
    found.iter().all(|(id, _)| id.starts_with(PREFIX)),
    "id_effect recorded an opt-out nobody took: {found:?}"
  );
  if std::env::var_os("NEXTEST").is_some() {
    assert!(found.is_empty(), "{found:?}");
  }
}

#[test]
fn posture_records_each_opt_out_once() {
  let a = "tests::posture::once::insecure_a";
  let b = "tests::posture::once::dangerous_b";
  let threads: Vec<_> = (0..8)
    .map(|_| {
      std::thread::spawn(move || {
        for _ in 0..100 {
          record_dangerous(b, "first b");
          record_dangerous(a, "first a");
        }
      })
    })
    .collect();
  for t in threads {
    t.join().unwrap();
  }
  record_dangerous(a, "second a");
  let found: Vec<_> = dangerous_opt_outs()
    .into_iter()
    .filter(|(id, _)| id.starts_with("tests::posture::once::"))
    .collect();
  assert_eq!(found, vec![(b, "first b"), (a, "first a")]);
  let all = dangerous_opt_outs();
  assert!(
    all.windows(2).all(|w| w[0].0 < w[1].0),
    "not sorted: {all:?}"
  );
}

#[test]
fn posture_lists_reason_text() {
  let id = "tests::posture::reason::insecure_plaintext_transport";
  let reopens = "R8: mesh traffic is neither encrypted nor mutually authenticated";
  assert!(!dangerous_opt_outs().iter().any(|(i, _)| *i == id));
  record_dangerous(id, reopens);
  let found = dangerous_opt_outs();
  let entry = found.iter().find(|(i, _)| *i == id);
  assert_eq!(entry, Some(&(id, reopens)));
}
