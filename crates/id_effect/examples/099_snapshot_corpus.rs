//! Ex 099 — Run the snapshot regression corpus shipped with the `effect` crate.
use id_effect::run_blocking;
use id_effect::testing::snapshot::{SNAPSHOT_CORPUS, snapshot_suite};

fn main() {
  let suite = snapshot_suite();
  assert_eq!(suite.len(), SNAPSHOT_CORPUS.len());
  for (idx, effect) in suite.into_iter().enumerate() {
    let snapshot = run_blocking(effect, ()).expect("snapshot effect should succeed");
    assert_eq!(snapshot.name, SNAPSHOT_CORPUS[idx]);
    assert!(
      snapshot.matches(),
      "snapshot mismatch: {} observed={} expected={}",
      snapshot.name,
      snapshot.observed,
      snapshot.expected
    );
  }
  println!("099_snapshot_corpus ok");
}
