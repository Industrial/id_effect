//! Additional targeted coverage for gate thresholds.

use id_effect::schema::parse::{ParseError, Unknown, bool_, i64, string};
use id_effect::streaming::join::{combine_latest, keyed_join};
use id_effect::streaming::stream::Stream;

fn block_on<F: core::future::Future>(fut: F) -> F::Output {
  pollster::block_on(fut)
}

#[test]
fn schema_primitives_decode_unknown() {
  assert_eq!(i64::<()>().decode_unknown(&Unknown::I64(9)).unwrap(), 9);
  assert_eq!(
    string::<()>()
      .decode_unknown(&Unknown::String("z".into()))
      .unwrap(),
    "z"
  );
  assert!(
    bool_::<()>()
      .decode_unknown(&Unknown::String("nope".into()))
      .is_err()
  );
  let err = ParseError::new("root", "bad").prefix("form");
  assert_eq!(err.path, "form.root");
}

#[test]
fn join_helpers_cover_late_matches() {
  let left = Stream::from_iterable([("k", 1)]);
  let right = Stream::from_iterable([("k", 'a'), ("other", 'b')]);
  let out = block_on(keyed_join(left, right).run_collect().run(&mut ())).expect("join");
  assert_eq!(out, vec![("k", 1, 'a')]);

  let l = Stream::from_iterable([1, 2, 3]);
  let r = Stream::from_iterable(['x']);
  let pairs = block_on(combine_latest(l, r).run_collect().run(&mut ())).expect("combine");
  assert_eq!(pairs.last().copied(), Some((3, 'x')));
}

#[test]
fn transducer_map_filter_transduce() {
  use id_effect::streaming::transducer::{filter, map};
  let xf = map(|x: i32| x * 2).compose(filter(|x| *x > 2));
  let sum = xf.transduce([1, 2, 3], Box::new(|acc: i32, x| acc + x), 0);
  assert_eq!(sum, 10);
}

use id_effect::runtime::run_blocking;
use id_effect::streaming::replay::broadcast_with_replay;

#[test]
fn replay_zero_branches_drains_upstream() {
  let stream = Stream::from_iterable([1, 2, 3]);
  let (outs, pump) = run_blocking(broadcast_with_replay(stream, 2, 1, 0), ()).expect("replay");
  assert!(outs.is_empty());
  run_blocking(pump, ()).expect("pump");
}
