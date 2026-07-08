#![allow(dead_code, clippy::new_ret_no_self)]

//! High-arity `CapList` verification and `project_at_*` coverage.

use id_effect::Cap;
use id_effect::capability::{CapList, CapWiden, CapabilitySet, Env, FromEnv};

macro_rules! cap {
  ($name:ident) => {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct $name(u32);
  };
}

cap!(C0);
cap!(C1);
cap!(C2);
cap!(C3);
cap!(C4);
cap!(C5);
cap!(C6);
cap!(C7);
cap!(C8);
cap!(C9);
cap!(C10);
cap!(C11);
cap!(C12);
cap!(C13);
cap!(C14);
cap!(C15);

type Keys16 = (
  Cap<C0>,
  Cap<C1>,
  Cap<C2>,
  Cap<C3>,
  Cap<C4>,
  Cap<C5>,
  Cap<C6>,
  Cap<C7>,
  Cap<C8>,
  Cap<C9>,
  Cap<C10>,
  Cap<C11>,
  Cap<C12>,
  Cap<C13>,
  Cap<C14>,
  Cap<C15>,
);

fn env16() -> Env {
  let mut env = Env::new();
  env.insert::<Cap<C0>>(C0(0));
  env.insert::<Cap<C1>>(C1(1));
  env.insert::<Cap<C2>>(C2(2));
  env.insert::<Cap<C3>>(C3(3));
  env.insert::<Cap<C4>>(C4(4));
  env.insert::<Cap<C5>>(C5(5));
  env.insert::<Cap<C6>>(C6(6));
  env.insert::<Cap<C7>>(C7(7));
  env.insert::<Cap<C8>>(C8(8));
  env.insert::<Cap<C9>>(C9(9));
  env.insert::<Cap<C10>>(C10(10));
  env.insert::<Cap<C11>>(C11(11));
  env.insert::<Cap<C12>>(C12(12));
  env.insert::<Cap<C13>>(C13(13));
  env.insert::<Cap<C14>>(C14(14));
  env.insert::<Cap<C15>>(C15(15));
  env
}

#[test]
fn cap_list_high_arity_integration() {
  CapList::<Keys16>::verify(&env16()).unwrap();
  assert!(CapList::<Keys16>::verify(&Env::new()).is_err());

  type Keys8 = (
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
    Cap<C7>,
  );
  let wide8 = CapList::<Keys8>::new(env16());
  let _ = wide8.clone().project_at_0();
  let _ = wide8.clone().project_at_1();
  let _ = wide8.clone().project_at_2();
  let _ = wide8.clone().project_at_3();
  let _ = wide8.clone().project_at_4();
  let _ = wide8.clone().project_at_5();
  let _ = wide8.clone().project_at_6();
  let _ = wide8.project_at_7();

  let env = env16();
  CapList::<(Cap<C0>,)>::verify(&env).unwrap();
  CapList::<(Cap<C0>, Cap<C1>)>::verify(&env).unwrap();
  CapList::<(Cap<C0>, Cap<C1>, Cap<C2>)>::verify(&env).unwrap();
  CapList::<(Cap<C0>, Cap<C1>, Cap<C2>, Cap<C3>)>::verify(&env).unwrap();
  CapList::<(Cap<C0>, Cap<C1>, Cap<C2>, Cap<C3>, Cap<C4>)>::verify(&env).unwrap();
  CapList::<(Cap<C0>, Cap<C1>, Cap<C2>, Cap<C3>, Cap<C4>, Cap<C5>)>::verify(&env).unwrap();
  CapList::<(
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
    Cap<C7>,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
    Cap<C7>,
    Cap<C8>,
    Cap<C9>,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
    Cap<C7>,
    Cap<C8>,
    Cap<C9>,
    Cap<C10>,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
    Cap<C7>,
    Cap<C8>,
    Cap<C9>,
    Cap<C10>,
    Cap<C11>,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
    Cap<C7>,
    Cap<C8>,
    Cap<C9>,
    Cap<C10>,
    Cap<C11>,
    Cap<C12>,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
    Cap<C7>,
    Cap<C8>,
    Cap<C9>,
    Cap<C10>,
    Cap<C11>,
    Cap<C12>,
    Cap<C13>,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    Cap<C0>,
    Cap<C1>,
    Cap<C2>,
    Cap<C3>,
    Cap<C4>,
    Cap<C5>,
    Cap<C6>,
    Cap<C7>,
    Cap<C8>,
    Cap<C9>,
    Cap<C10>,
    Cap<C11>,
    Cap<C12>,
    Cap<C13>,
    Cap<C14>,
  )>::verify(&env)
  .unwrap();

  let mut partial = Env::new();
  assert!(CapList::<(Cap<C0>,)>::verify(&partial).is_err());
  partial.insert::<Cap<C0>>(C0(0));
  assert!(CapList::<(Cap<C0>, Cap<C1>)>::verify(&partial).is_err());
  partial.insert::<Cap<C1>>(C1(1));
  assert!(CapList::<(Cap<C0>, Cap<C1>, Cap<C2>)>::verify(&partial).is_err());

  let mut env15 = Env::new();
  env15.insert::<Cap<C0>>(C0(0));
  env15.insert::<Cap<C1>>(C1(1));
  env15.insert::<Cap<C2>>(C2(2));
  env15.insert::<Cap<C3>>(C3(3));
  env15.insert::<Cap<C4>>(C4(4));
  env15.insert::<Cap<C5>>(C5(5));
  env15.insert::<Cap<C6>>(C6(6));
  env15.insert::<Cap<C7>>(C7(7));
  env15.insert::<Cap<C8>>(C8(8));
  env15.insert::<Cap<C9>>(C9(9));
  env15.insert::<Cap<C10>>(C10(10));
  env15.insert::<Cap<C11>>(C11(11));
  env15.insert::<Cap<C12>>(C12(12));
  env15.insert::<Cap<C13>>(C13(13));
  env15.insert::<Cap<C14>>(C14(14));
  assert!(CapList::<Keys16>::verify(&env15).is_err());

  let mut caps = CapList::<(Cap<C0>, Cap<C1>)>::from_env({
    let mut e = Env::new();
    e.insert::<Cap<C0>>(C0(10));
    e.insert::<Cap<C1>>(C1(20));
    e
  });
  assert_eq!(caps.get::<Cap<C0>>().0, 10);
  caps.env_mut().insert::<Cap<C1>>(C1(21));
  assert_eq!(caps.get::<Cap<C1>>().0, 21);

  type Keys3 = (Cap<C0>, Cap<C1>, Cap<C2>);
  let wide3 = <CapList<Keys3> as FromEnv>::from_env(env16());
  let one: CapList<(Cap<C0>,)> = CapWiden::widen(wide3.clone());
  CapList::<(Cap<C0>,)>::verify(one.env()).unwrap();
  let two: CapList<(Cap<C0>, Cap<C1>)> = CapWiden::widen(wide3);
  CapList::<(Cap<C0>, Cap<C1>)>::verify(two.env()).unwrap();
}
