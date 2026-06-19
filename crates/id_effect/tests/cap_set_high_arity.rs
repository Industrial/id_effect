#![allow(dead_code, clippy::new_ret_no_self)]

//! High-arity `CapList` verification and `project_at_*` coverage.

use id_effect::capability::{CapList, CapWiden, CapabilitySet, Env, FromEnv};

macro_rules! cap {
  ($name:ident) => {
    #[::id_effect::capability(u8)]
    struct $name;
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
  C0Key,
  C1Key,
  C2Key,
  C3Key,
  C4Key,
  C5Key,
  C6Key,
  C7Key,
  C8Key,
  C9Key,
  C10Key,
  C11Key,
  C12Key,
  C13Key,
  C14Key,
  C15Key,
);

fn env16() -> Env {
  let mut env = Env::new();
  env.insert::<C0Key>(0);
  env.insert::<C1Key>(1);
  env.insert::<C2Key>(2);
  env.insert::<C3Key>(3);
  env.insert::<C4Key>(4);
  env.insert::<C5Key>(5);
  env.insert::<C6Key>(6);
  env.insert::<C7Key>(7);
  env.insert::<C8Key>(8);
  env.insert::<C9Key>(9);
  env.insert::<C10Key>(10);
  env.insert::<C11Key>(11);
  env.insert::<C12Key>(12);
  env.insert::<C13Key>(13);
  env.insert::<C14Key>(14);
  env.insert::<C15Key>(15);
  env
}

#[test]
fn cap_list_high_arity_integration() {
  CapList::<Keys16>::verify(&env16()).unwrap();
  assert!(CapList::<Keys16>::verify(&Env::new()).is_err());

  type Keys8 = (C0Key, C1Key, C2Key, C3Key, C4Key, C5Key, C6Key, C7Key);
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
  CapList::<(C0Key,)>::verify(&env).unwrap();
  CapList::<(C0Key, C1Key)>::verify(&env).unwrap();
  CapList::<(C0Key, C1Key, C2Key)>::verify(&env).unwrap();
  CapList::<(C0Key, C1Key, C2Key, C3Key)>::verify(&env).unwrap();
  CapList::<(C0Key, C1Key, C2Key, C3Key, C4Key)>::verify(&env).unwrap();
  CapList::<(C0Key, C1Key, C2Key, C3Key, C4Key, C5Key)>::verify(&env).unwrap();
  CapList::<(C0Key, C1Key, C2Key, C3Key, C4Key, C5Key, C6Key)>::verify(&env).unwrap();
  CapList::<(C0Key, C1Key, C2Key, C3Key, C4Key, C5Key, C6Key, C7Key)>::verify(&env).unwrap();
  CapList::<(
    C0Key,
    C1Key,
    C2Key,
    C3Key,
    C4Key,
    C5Key,
    C6Key,
    C7Key,
    C8Key,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    C0Key,
    C1Key,
    C2Key,
    C3Key,
    C4Key,
    C5Key,
    C6Key,
    C7Key,
    C8Key,
    C9Key,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    C0Key,
    C1Key,
    C2Key,
    C3Key,
    C4Key,
    C5Key,
    C6Key,
    C7Key,
    C8Key,
    C9Key,
    C10Key,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    C0Key,
    C1Key,
    C2Key,
    C3Key,
    C4Key,
    C5Key,
    C6Key,
    C7Key,
    C8Key,
    C9Key,
    C10Key,
    C11Key,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    C0Key,
    C1Key,
    C2Key,
    C3Key,
    C4Key,
    C5Key,
    C6Key,
    C7Key,
    C8Key,
    C9Key,
    C10Key,
    C11Key,
    C12Key,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    C0Key,
    C1Key,
    C2Key,
    C3Key,
    C4Key,
    C5Key,
    C6Key,
    C7Key,
    C8Key,
    C9Key,
    C10Key,
    C11Key,
    C12Key,
    C13Key,
  )>::verify(&env)
  .unwrap();
  CapList::<(
    C0Key,
    C1Key,
    C2Key,
    C3Key,
    C4Key,
    C5Key,
    C6Key,
    C7Key,
    C8Key,
    C9Key,
    C10Key,
    C11Key,
    C12Key,
    C13Key,
    C14Key,
  )>::verify(&env)
  .unwrap();

  let mut partial = Env::new();
  assert!(CapList::<(C0Key,)>::verify(&partial).is_err());
  partial.insert::<C0Key>(0);
  assert!(CapList::<(C0Key, C1Key)>::verify(&partial).is_err());
  partial.insert::<C1Key>(1);
  assert!(CapList::<(C0Key, C1Key, C2Key)>::verify(&partial).is_err());

  let mut env15 = Env::new();
  env15.insert::<C0Key>(0);
  env15.insert::<C1Key>(1);
  env15.insert::<C2Key>(2);
  env15.insert::<C3Key>(3);
  env15.insert::<C4Key>(4);
  env15.insert::<C5Key>(5);
  env15.insert::<C6Key>(6);
  env15.insert::<C7Key>(7);
  env15.insert::<C8Key>(8);
  env15.insert::<C9Key>(9);
  env15.insert::<C10Key>(10);
  env15.insert::<C11Key>(11);
  env15.insert::<C12Key>(12);
  env15.insert::<C13Key>(13);
  env15.insert::<C14Key>(14);
  assert!(CapList::<Keys16>::verify(&env15).is_err());

  let mut caps = CapList::<(C0Key, C1Key)>::from_env({
    let mut e = Env::new();
    e.insert::<C0Key>(10);
    e.insert::<C1Key>(20);
    e
  });
  assert_eq!(*caps.get::<C0Key>(), 10);
  caps.env_mut().insert::<C1Key>(21);
  assert_eq!(*caps.get::<C1Key>(), 21);

  type Keys3 = (C0Key, C1Key, C2Key);
  let wide3 = <CapList<Keys3> as FromEnv>::from_env(env16());
  let one: CapList<(C0Key,)> = CapWiden::widen(wide3.clone());
  CapList::<(C0Key,)>::verify(one.env()).unwrap();
  let two: CapList<(C0Key, C1Key)> = CapWiden::widen(wide3);
  CapList::<(C0Key, C1Key)>::verify(two.env()).unwrap();
}
