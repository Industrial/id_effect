#![allow(dead_code, clippy::new_ret_no_self)]

//! Proptest: `project_at_*` + `CapList` verification matches inserted keys.

use id_effect::Cap;
use id_effect::capability::{CapList, CapabilitySet, Env};
use proptest::prelude::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct C0(u32);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct C1(u32);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct C2(u32);

fn env_with(mask: u8) -> Env {
  let mut env = Env::new();
  if mask & 1 != 0 {
    env.insert::<Cap<C0>>(C0(0));
  }
  if mask & 2 != 0 {
    env.insert::<Cap<C1>>(C1(1));
  }
  if mask & 4 != 0 {
    env.insert::<Cap<C2>>(C2(2));
  }
  env
}

proptest! {
  #[test]
  fn project_at_verify_matches_inserted_keys(mask in 0u8..8u8) {
    let env = env_with(mask);
    let wide = CapList::<(Cap<C0>, Cap<C1>, Cap<C2>)>::new(env);

    let narrow0 = wide.clone().project_at_0();
    let ok0 = CapList::<(Cap<C0>,)>::verify(narrow0.env()).is_ok();
    prop_assert_eq!(ok0, mask & 1 != 0);

    let narrow1 = wide.clone().project_at_1();
    let ok1 = CapList::<(Cap<C1>,)>::verify(narrow1.env()).is_ok();
    prop_assert_eq!(ok1, mask & 2 != 0);

    let narrow2 = wide.clone().project_at_2();
    let ok2 = CapList::<(Cap<C2>,)>::verify(narrow2.env()).is_ok();
    prop_assert_eq!(ok2, mask & 4 != 0);
  }
}
