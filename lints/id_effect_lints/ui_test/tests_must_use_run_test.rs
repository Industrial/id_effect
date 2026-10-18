// D-02 (Allow by default, enabled here): `block_on` in a test. Compiled with `--test`.
#![allow(dead_code)]
#![warn(tests_must_use_run_test)]
use std::marker::PhantomData;

struct Effect<X, Y, Z>(PhantomData<(X, Y, Z)>);

impl<X, Y, Z> Effect<X, Y, Z> {
  fn block_on(self) {}
}

fn ready() -> Effect<u8, (), ()> {
  Effect(PhantomData)
}

#[test]
fn runs_on_the_real_clock() {
  ready().block_on();
}
