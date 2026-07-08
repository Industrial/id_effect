#![allow(dead_code, clippy::new_ret_no_self)]

//! Exercise every `project_at_*` generated for arities 2–8.

use id_effect::Cap;
use id_effect::capability::{CapList, Env, FromEnv};

macro_rules! cap {
  ($name:ident) => {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct $name(u32);
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
  env.insert::<Cap<P0>>(P0(0));
  env.insert::<Cap<P1>>(P1(1));
  env.insert::<Cap<P2>>(P2(2));
  env.insert::<Cap<P3>>(P3(3));
  env.insert::<Cap<P4>>(P4(4));
  env.insert::<Cap<P5>>(P5(5));
  env.insert::<Cap<P6>>(P6(6));
  env.insert::<Cap<P7>>(P7(7));
  env
}

#[test]
fn project_at_all_arity_two_through_eight() {
  type K2 = (Cap<P0>, Cap<P1>);
  type K3 = (Cap<P0>, Cap<P1>, Cap<P2>);
  type K4 = (Cap<P0>, Cap<P1>, Cap<P2>, Cap<P3>);
  type K5 = (Cap<P0>, Cap<P1>, Cap<P2>, Cap<P3>, Cap<P4>);
  type K6 = (Cap<P0>, Cap<P1>, Cap<P2>, Cap<P3>, Cap<P4>, Cap<P5>);
  type K7 = (
    Cap<P0>,
    Cap<P1>,
    Cap<P2>,
    Cap<P3>,
    Cap<P4>,
    Cap<P5>,
    Cap<P6>,
  );
  type K8 = (
    Cap<P0>,
    Cap<P1>,
    Cap<P2>,
    Cap<P3>,
    Cap<P4>,
    Cap<P5>,
    Cap<P6>,
    Cap<P7>,
  );

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
