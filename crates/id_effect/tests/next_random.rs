//! `p-random` acceptance tests for `id_effect::next::{Random, RandomSource, TestRandom}` and the
//! `random_*` effect accessors.
//!
//! The golden sequence for seed 42 was computed with an independent Python model of SplitMix64
//! seeding + xoshiro256** (not committed); the same model reproduces the reference vector for
//! state `[1, 2, 3, 4]` and SplitMix64's first output for seed 0, both checked in the unit tests
//! of `next/random.rs`.

use std::collections::HashSet;

use id_effect::Exit;
use id_effect::next::{
  Effect, HCons, HNil, Random, RandomSource, TestRandom, need, random_f64, random_range,
  random_shuffle, random_u64, shuffle_with,
};

/// `seeded(42)`: SplitMix64(42) fills the state, then xoshiro256**.
const GOLDEN_42: [u64; 8] = [
  1_546_998_764_402_558_742,
  6_990_951_692_964_543_102,
  12_544_586_762_248_559_009,
  17_057_574_109_182_124_193,
  18_295_552_978_065_317_476,
  14_199_186_830_065_750_584,
  13_267_978_908_934_200_754,
  15_679_888_225_317_814_407,
];

fn draw(src: &dyn RandomSource, n: usize) -> Vec<u64> {
  (0..n).map(|_| src.next_u64()).collect()
}

fn success<A>(exit: Exit<A, ()>) -> A {
  match exit {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("expected success, got {c:?}"),
  }
}

// ── golden sequence ──────────────────────────────────────────────────────────────────────────

#[test]
fn test_random_seeded_42_matches_the_golden_sequence() {
  assert_eq!(draw(&TestRandom::seeded(42), 8), GOLDEN_42);
}

#[test]
fn random_seeded_gives_the_same_sequence_as_test_random() {
  assert_eq!(draw(&Random::seeded(42), 8), GOLDEN_42);
  assert_eq!(
    draw(&Random::seeded(9), 16),
    draw(&TestRandom::seeded(9), 16)
  );
  assert_ne!(draw(&Random::seeded(1), 4), draw(&Random::seeded(2), 4));
}

#[test]
fn next_f64_is_the_top_53_bits_of_next_u64() {
  let rng = TestRandom::seeded(42);
  let scale = 1.0 / (1u64 << 53) as f64;
  for g in GOLDEN_42 {
    assert_eq!(rng.next_f64(), (g >> 11) as f64 * scale);
  }
}

// ── next_f64 / next_range / shuffle ─────────────────────────────────────────────────────────

#[test]
fn next_f64_is_in_unit_interval_and_roughly_uniform() {
  let rng = Random::seeded(3);
  let n = 100_000;
  let mut sum = 0.0;
  for _ in 0..n {
    let x = rng.next_f64();
    assert!((0.0..1.0).contains(&x), "{x} not in [0, 1)");
    sum += x;
  }
  let mean = sum / f64::from(n);
  assert!((mean - 0.5).abs() < 0.01, "mean {mean}");
}

#[test]
fn next_range_stays_in_range_and_covers_it() {
  let rng = Random::seeded(5);
  let mut seen = HashSet::new();
  for _ in 0..10_000 {
    let v = rng.next_range(10..17);
    assert!((10..17).contains(&v));
    seen.insert(v);
  }
  assert_eq!(seen.len(), 7);

  let mut counts = [0_u32; 3];
  for _ in 0..30_000 {
    counts[rng.next_range(0..3) as usize] += 1;
  }
  for c in counts {
    assert!((9_000..11_000).contains(&c), "{counts:?}");
  }
}

#[test]
fn next_range_edges() {
  let rng = Random::seeded(6);
  assert_eq!(rng.next_range(4..4), 4);
  #[allow(
    clippy::reversed_empty_ranges,
    reason = "a reversed range is the case under test"
  )]
  let reversed = 9..2;
  assert_eq!(rng.next_range(reversed), 9);
  assert_eq!(rng.next_range(7..8), 7);
  for _ in 0..1_000 {
    assert_eq!(rng.next_range(u64::MAX - 1..u64::MAX), u64::MAX - 1);
    assert!(rng.next_range(0..u64::MAX) < u64::MAX);
  }
}

#[test]
fn an_empty_range_draws_nothing() {
  let rng = TestRandom::seeded(42);
  assert_eq!(rng.next_range(3..3), 3);
  assert_eq!(rng.next_u64(), GOLDEN_42[0]);
}

#[test]
fn shuffle_is_a_deterministic_permutation() {
  let original: Vec<u32> = (0..20).collect();
  let mut a = original.clone();
  let mut b = original.clone();
  Random::seeded(42).shuffle(&mut a);
  shuffle_with(&TestRandom::seeded(42), &mut b);
  assert_eq!(a, b, "same seed, same permutation");
  assert_ne!(a, original);
  let mut sorted = a.clone();
  sorted.sort_unstable();
  assert_eq!(sorted, original);

  let mut empty: [u8; 0] = [];
  Random::seeded(1).shuffle(&mut empty);
  let mut one = [9];
  Random::seeded(1).shuffle(&mut one);
  assert_eq!(one, [9]);
}

#[test]
fn shuffle_reaches_every_permutation_of_three() {
  let rng = Random::seeded(11);
  let mut seen = HashSet::new();
  for _ in 0..600 {
    let mut v = ['a', 'b', 'c'];
    rng.shuffle(&mut v);
    seen.insert(v);
  }
  assert_eq!(seen.len(), 6);
}

// ── TestRandom::feed ─────────────────────────────────────────────────────────────────────────

#[test]
fn fed_u64s_come_back_first_without_advancing_the_generator() {
  let rng = TestRandom::seeded(42);
  rng.feed_u64s([1, 2]);
  rng.feed_u64s([3]);
  assert_eq!(draw(&rng, 5), [1, 2, 3, GOLDEN_42[0], GOLDEN_42[1]]);
}

#[test]
fn fed_f64s_come_back_first_and_are_clamped_into_the_unit_interval() {
  let rng = TestRandom::seeded(42);
  rng.feed_f64s([0.25, 0.75, -1.0, f64::NAN, 1.0, 7.5]);
  let got: Vec<f64> = (0..6).map(|_| rng.next_f64()).collect();
  assert_eq!(&got[..4], [0.25, 0.75, 0.0, 0.0]);
  assert!(got[4] < 1.0 && got[4] > 0.999_999);
  assert_eq!(got[4], got[5]);
  // The queues are independent: the f64 feed did not touch the u64 sequence.
  assert_eq!(rng.next_u64(), GOLDEN_42[0]);
}

#[test]
fn fed_u64s_drive_next_range_and_shuffle() {
  let rng = TestRandom::seeded(42);
  rng.feed_u64s([5, 12]);
  assert_eq!(rng.next_range(0..10), 5, "in range: verbatim");
  assert_eq!(rng.next_range(0..10), 2, "out of range: reduced");

  // Fisher–Yates over 3 items draws next_range(0..3) then next_range(0..2).
  rng.feed_u64s([0, 0]);
  let mut v = ['a', 'b', 'c'];
  shuffle_with(&rng, &mut v);
  assert_eq!(v, ['b', 'c', 'a']);
}

#[test]
fn clear_feeds_drops_queued_values() {
  let rng = TestRandom::seeded(42);
  rng.feed_u64s([1]);
  rng.feed_f64s([0.5]);
  rng.clear_feeds();
  assert_eq!(rng.next_u64(), GOLDEN_42[0]);
}

#[test]
fn clones_share_one_sequence() {
  let rng = TestRandom::seeded(42);
  let other = rng.clone();
  let cap = rng.random();
  assert_eq!(rng.next_u64(), GOLDEN_42[0]);
  assert_eq!(other.next_u64(), GOLDEN_42[1]);
  assert_eq!(cap.next_u64(), GOLDEN_42[2]);
  other.feed_u64s([77]);
  assert_eq!(cap.next_u64(), 77);
}

// ── as a capability ──────────────────────────────────────────────────────────────────────────

struct Label(&'static str);

type Env = HCons<Label, HCons<Random, HNil>>;

fn program() -> Effect<(String, u64, f64, Vec<u8>), (), Env> {
  need::<Label>().flat_map(|label| {
    random_range(0..100).flat_map(move |n| {
      random_f64().flat_map(move |x| {
        random_shuffle(vec![1, 2, 3, 4]).map(move |v| (label.0.to_owned(), n, x, v))
      })
    })
  })
}

#[test]
fn random_is_read_from_the_environment() {
  let rng = TestRandom::seeded(42);
  rng.feed_u64s([42, 0, 0, 0]);
  rng.feed_f64s([0.5]);
  let exit = program()
    .provide(Label("roll"))
    .provide(Random::from(rng.clone()))
    .block_on();
  let (label, n, x, v) = success(exit);
  assert_eq!((label.as_str(), n, x), ("roll", 42, 0.5));
  assert_eq!(v, [2, 3, 4, 1]);
  // Every fed value was used; the generator is untouched.
  assert_eq!(rng.next_u64(), GOLDEN_42[0]);
}

#[test]
fn random_u64_under_a_seeded_capability_is_reproducible() {
  let run = |seed| {
    let eff: Effect<Vec<u64>, (), HCons<Random, HNil>> =
      random_u64().flat_map(|a| random_u64().map(move |b| vec![a, b]));
    success(eff.provide(Random::seeded(seed)).block_on())
  };
  assert_eq!(run(42), GOLDEN_42[..2]);
}

#[test]
fn live_random_is_a_send_sync_capability() {
  fn assert_send_sync<T: Send + Sync + 'static>() {}
  assert_send_sync::<Random>();
  assert_send_sync::<TestRandom>();

  let rng = Random::live();
  let handles: Vec<_> = (0..4)
    .map(|_| {
      let rng = rng.clone();
      std::thread::spawn(move || (0..1_000).map(|_| rng.next_u64()).collect::<Vec<_>>())
    })
    .collect();
  let all: HashSet<u64> = handles
    .into_iter()
    .flat_map(|h| h.join().expect("thread"))
    .collect();
  assert_eq!(
    all.len(),
    4_000,
    "clones draw from one sequence, no repeats"
  );
  assert!(format!("{rng:?}").starts_with("Random"));
}
