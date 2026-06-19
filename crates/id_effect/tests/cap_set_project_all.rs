#![allow(dead_code, clippy::new_ret_no_self)]

//! Exercise every `project_at_*` generated for arities 2–8.

use id_effect::capability::{CapList, Env, FromEnv};

macro_rules! cap {
  ($name:ident) => {
    #[::id_effect::capability(u8)]
    struct $name;
  };
}

cap!(P0);
cap!(P1);
cap!(P2);
cap!(P3);
cap!(P4);
cap!(P5);
cap!(P6);
cap!(P7);

fn env8() -> Env {
  let mut env = Env::new();
  env.insert::<P0Key>(0);
  env.insert::<P1Key>(1);
  env.insert::<P2Key>(2);
  env.insert::<P3Key>(3);
  env.insert::<P4Key>(4);
  env.insert::<P5Key>(5);
  env.insert::<P6Key>(6);
  env.insert::<P7Key>(7);
  env
}

#[test]
fn project_at_all_arity_two_through_eight() {
  type K2 = (P0Key, P1Key);
  type K3 = (P0Key, P1Key, P2Key);
  type K4 = (P0Key, P1Key, P2Key, P3Key);
  type K5 = (P0Key, P1Key, P2Key, P3Key, P4Key);
  type K6 = (P0Key, P1Key, P2Key, P3Key, P4Key, P5Key);
  type K7 = (P0Key, P1Key, P2Key, P3Key, P4Key, P5Key, P6Key);
  type K8 = (P0Key, P1Key, P2Key, P3Key, P4Key, P5Key, P6Key, P7Key);

  let env = env8();

  let w2 = CapList::<K2>::from_env(env.clone());
  let _ = w2.clone().project_at_0();
  let _ = w2.project_at_1();

  let w3 = CapList::<K3>::from_env(env.clone());
  let _ = w3.clone().project_at_0();
  let _ = w3.clone().project_at_1();
  let _ = w3.project_at_2();

  let w4 = CapList::<K4>::from_env(env.clone());
  let _ = w4.clone().project_at_0();
  let _ = w4.clone().project_at_1();
  let _ = w4.clone().project_at_2();
  let _ = w4.project_at_3();

  let w5 = CapList::<K5>::from_env(env.clone());
  let _ = w5.clone().project_at_0();
  let _ = w5.clone().project_at_1();
  let _ = w5.clone().project_at_2();
  let _ = w5.clone().project_at_3();
  let _ = w5.project_at_4();

  let w6 = CapList::<K6>::from_env(env.clone());
  let _ = w6.clone().project_at_0();
  let _ = w6.clone().project_at_1();
  let _ = w6.clone().project_at_2();
  let _ = w6.clone().project_at_3();
  let _ = w6.clone().project_at_4();
  let _ = w6.project_at_5();

  let w7 = CapList::<K7>::from_env(env.clone());
  let _ = w7.clone().project_at_0();
  let _ = w7.clone().project_at_1();
  let _ = w7.clone().project_at_2();
  let _ = w7.clone().project_at_3();
  let _ = w7.clone().project_at_4();
  let _ = w7.clone().project_at_5();
  let _ = w7.project_at_6();

  let w8 = CapList::<K8>::from_env(env);
  let _ = w8.clone().project_at_0();
  let _ = w8.clone().project_at_1();
  let _ = w8.clone().project_at_2();
  let _ = w8.clone().project_at_3();
  let _ = w8.clone().project_at_4();
  let _ = w8.clone().project_at_5();
  let _ = w8.clone().project_at_6();
  let _ = w8.project_at_7();
}
