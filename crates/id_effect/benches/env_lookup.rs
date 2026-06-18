#![allow(dead_code)]
//! Env insert/get/has microbenchmarks at N = 4, 16, 64 capabilities.

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect::Env;

#[::id_effect::capability(u32)]
struct Cap0;

#[::id_effect::capability(u32)]
struct Cap1;

#[::id_effect::capability(u32)]
struct Cap2;

#[::id_effect::capability(u32)]
struct Cap3;

#[::id_effect::capability(u32)]
struct Cap4;

#[::id_effect::capability(u32)]
struct Cap5;

#[::id_effect::capability(u32)]
struct Cap6;

#[::id_effect::capability(u32)]
struct Cap7;

#[::id_effect::capability(u32)]
struct Cap8;

#[::id_effect::capability(u32)]
struct Cap9;

#[::id_effect::capability(u32)]
struct Cap10;

#[::id_effect::capability(u32)]
struct Cap11;

#[::id_effect::capability(u32)]
struct Cap12;

#[::id_effect::capability(u32)]
struct Cap13;

#[::id_effect::capability(u32)]
struct Cap14;

#[::id_effect::capability(u32)]
struct Cap15;

#[::id_effect::capability(u32)]
struct Cap16;

#[::id_effect::capability(u32)]
struct Cap17;

#[::id_effect::capability(u32)]
struct Cap18;

#[::id_effect::capability(u32)]
struct Cap19;

#[::id_effect::capability(u32)]
struct Cap20;

#[::id_effect::capability(u32)]
struct Cap21;

#[::id_effect::capability(u32)]
struct Cap22;

#[::id_effect::capability(u32)]
struct Cap23;

#[::id_effect::capability(u32)]
struct Cap24;

#[::id_effect::capability(u32)]
struct Cap25;

#[::id_effect::capability(u32)]
struct Cap26;

#[::id_effect::capability(u32)]
struct Cap27;

#[::id_effect::capability(u32)]
struct Cap28;

#[::id_effect::capability(u32)]
struct Cap29;

#[::id_effect::capability(u32)]
struct Cap30;

#[::id_effect::capability(u32)]
struct Cap31;

#[::id_effect::capability(u32)]
struct Cap32;

#[::id_effect::capability(u32)]
struct Cap33;

#[::id_effect::capability(u32)]
struct Cap34;

#[::id_effect::capability(u32)]
struct Cap35;

#[::id_effect::capability(u32)]
struct Cap36;

#[::id_effect::capability(u32)]
struct Cap37;

#[::id_effect::capability(u32)]
struct Cap38;

#[::id_effect::capability(u32)]
struct Cap39;

#[::id_effect::capability(u32)]
struct Cap40;

#[::id_effect::capability(u32)]
struct Cap41;

#[::id_effect::capability(u32)]
struct Cap42;

#[::id_effect::capability(u32)]
struct Cap43;

#[::id_effect::capability(u32)]
struct Cap44;

#[::id_effect::capability(u32)]
struct Cap45;

#[::id_effect::capability(u32)]
struct Cap46;

#[::id_effect::capability(u32)]
struct Cap47;

#[::id_effect::capability(u32)]
struct Cap48;

#[::id_effect::capability(u32)]
struct Cap49;

#[::id_effect::capability(u32)]
struct Cap50;

#[::id_effect::capability(u32)]
struct Cap51;

#[::id_effect::capability(u32)]
struct Cap52;

#[::id_effect::capability(u32)]
struct Cap53;

#[::id_effect::capability(u32)]
struct Cap54;

#[::id_effect::capability(u32)]
struct Cap55;

#[::id_effect::capability(u32)]
struct Cap56;

#[::id_effect::capability(u32)]
struct Cap57;

#[::id_effect::capability(u32)]
struct Cap58;

#[::id_effect::capability(u32)]
struct Cap59;

#[::id_effect::capability(u32)]
struct Cap60;

#[::id_effect::capability(u32)]
struct Cap61;

#[::id_effect::capability(u32)]
struct Cap62;

#[::id_effect::capability(u32)]
struct Cap63;

fn bench_env_n(c: &mut Criterion, name: &str, setup: impl Fn() -> Env, get: fn(&Env) -> u32) {
  c.bench_function(name, |b| {
    let env = setup();
    b.iter(|| std::hint::black_box(get(&env)));
  });
}

fn bench_env_lookup(c: &mut Criterion) {
  bench_env_n(
    c,
    "env_get_n4",
    || {
      let mut env = Env::new();
      env.insert::<Cap0Key>(0);
      env.insert::<Cap1Key>(0);
      env.insert::<Cap2Key>(0);
      env.insert::<Cap3Key>(0);
      env
    },
    |e| *e.get::<Cap3Key>(),
  );

  bench_env_n(
    c,
    "env_get_n16",
    || {
      let mut env = Env::new();
      env.insert::<Cap0Key>(0);
      env.insert::<Cap1Key>(0);
      env.insert::<Cap2Key>(0);
      env.insert::<Cap3Key>(0);
      env.insert::<Cap4Key>(0);
      env.insert::<Cap5Key>(0);
      env.insert::<Cap6Key>(0);
      env.insert::<Cap7Key>(0);
      env.insert::<Cap8Key>(0);
      env.insert::<Cap9Key>(0);
      env.insert::<Cap10Key>(0);
      env.insert::<Cap11Key>(0);
      env.insert::<Cap12Key>(0);
      env.insert::<Cap13Key>(0);
      env.insert::<Cap14Key>(0);
      env.insert::<Cap15Key>(0);
      env
    },
    |e| *e.get::<Cap15Key>(),
  );

  bench_env_n(
    c,
    "env_get_n64",
    || {
      let mut env = Env::new();
      env.insert::<Cap0Key>(0);
      env.insert::<Cap1Key>(0);
      env.insert::<Cap2Key>(0);
      env.insert::<Cap3Key>(0);
      env.insert::<Cap4Key>(0);
      env.insert::<Cap5Key>(0);
      env.insert::<Cap6Key>(0);
      env.insert::<Cap7Key>(0);
      env.insert::<Cap8Key>(0);
      env.insert::<Cap9Key>(0);
      env.insert::<Cap10Key>(0);
      env.insert::<Cap11Key>(0);
      env.insert::<Cap12Key>(0);
      env.insert::<Cap13Key>(0);
      env.insert::<Cap14Key>(0);
      env.insert::<Cap15Key>(0);
      env.insert::<Cap16Key>(0);
      env.insert::<Cap17Key>(0);
      env.insert::<Cap18Key>(0);
      env.insert::<Cap19Key>(0);
      env.insert::<Cap20Key>(0);
      env.insert::<Cap21Key>(0);
      env.insert::<Cap22Key>(0);
      env.insert::<Cap23Key>(0);
      env.insert::<Cap24Key>(0);
      env.insert::<Cap25Key>(0);
      env.insert::<Cap26Key>(0);
      env.insert::<Cap27Key>(0);
      env.insert::<Cap28Key>(0);
      env.insert::<Cap29Key>(0);
      env.insert::<Cap30Key>(0);
      env.insert::<Cap31Key>(0);
      env.insert::<Cap32Key>(0);
      env.insert::<Cap33Key>(0);
      env.insert::<Cap34Key>(0);
      env.insert::<Cap35Key>(0);
      env.insert::<Cap36Key>(0);
      env.insert::<Cap37Key>(0);
      env.insert::<Cap38Key>(0);
      env.insert::<Cap39Key>(0);
      env.insert::<Cap40Key>(0);
      env.insert::<Cap41Key>(0);
      env.insert::<Cap42Key>(0);
      env.insert::<Cap43Key>(0);
      env.insert::<Cap44Key>(0);
      env.insert::<Cap45Key>(0);
      env.insert::<Cap46Key>(0);
      env.insert::<Cap47Key>(0);
      env.insert::<Cap48Key>(0);
      env.insert::<Cap49Key>(0);
      env.insert::<Cap50Key>(0);
      env.insert::<Cap51Key>(0);
      env.insert::<Cap52Key>(0);
      env.insert::<Cap53Key>(0);
      env.insert::<Cap54Key>(0);
      env.insert::<Cap55Key>(0);
      env.insert::<Cap56Key>(0);
      env.insert::<Cap57Key>(0);
      env.insert::<Cap58Key>(0);
      env.insert::<Cap59Key>(0);
      env.insert::<Cap60Key>(0);
      env.insert::<Cap61Key>(0);
      env.insert::<Cap62Key>(0);
      env.insert::<Cap63Key>(0);
      env
    },
    |e| *e.get::<Cap63Key>(),
  );
}

criterion_group!(benches, bench_env_lookup);
criterion_main!(benches);
