//! Ex 071 — `map_serial` when a closure captures mutable state (not `Send`).
//!
//! Default `Stream::map` uses Rayon on large chunks; `map_serial` keeps chunk work
//! on the current thread with `FnMut` closures.
use id_effect::{Stream, run_blocking};

fn main() {
  let mut offset = 0_i32;
  let s = Stream::from_iterable(1..=8).map_serial(move |n| {
    offset += 1;
    n + offset
  });
  let v = run_blocking(s.run_collect(), ()).expect("collect");
  assert_eq!(v, vec![2, 4, 6, 8, 10, 12, 14, 16]);
  println!("071_stream_map_serial ok");
}
