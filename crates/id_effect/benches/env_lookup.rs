#![allow(dead_code)]
//! Env insert/get/has microbenchmarks at N = 4, 16, 64 capabilities.
//!
//! Kernel measured: the 0.7 runtime-checked environment `id_effect::next::DynEnv` (v0.6:
//! `Env` with `Cap<T>` keys). IDs are unchanged (`env_get_n{4,16,64}`); the workload is the
//! same: build an environment of N capabilities once, then look up the last-inserted one per
//! iteration. `DynEnv::try_get` returns an `Arc<T>` (one refcount bump and drop per lookup)
//! where v0.6 `Env::get` returned `&T`.

use criterion::{Criterion, criterion_group, criterion_main};
use id_effect::next::DynEnv;

#[derive(Clone, Copy)]
struct Cap0(u32);
#[derive(Clone, Copy)]
struct Cap1(u32);
#[derive(Clone, Copy)]
struct Cap2(u32);
#[derive(Clone, Copy)]
struct Cap3(u32);
#[derive(Clone, Copy)]
struct Cap4(u32);
#[derive(Clone, Copy)]
struct Cap5(u32);
#[derive(Clone, Copy)]
struct Cap6(u32);
#[derive(Clone, Copy)]
struct Cap7(u32);
#[derive(Clone, Copy)]
struct Cap8(u32);
#[derive(Clone, Copy)]
struct Cap9(u32);
#[derive(Clone, Copy)]
struct Cap10(u32);
#[derive(Clone, Copy)]
struct Cap11(u32);
#[derive(Clone, Copy)]
struct Cap12(u32);
#[derive(Clone, Copy)]
struct Cap13(u32);
#[derive(Clone, Copy)]
struct Cap14(u32);
#[derive(Clone, Copy)]
struct Cap15(u32);
#[derive(Clone, Copy)]
struct Cap16(u32);
#[derive(Clone, Copy)]
struct Cap17(u32);
#[derive(Clone, Copy)]
struct Cap18(u32);
#[derive(Clone, Copy)]
struct Cap19(u32);
#[derive(Clone, Copy)]
struct Cap20(u32);
#[derive(Clone, Copy)]
struct Cap21(u32);
#[derive(Clone, Copy)]
struct Cap22(u32);
#[derive(Clone, Copy)]
struct Cap23(u32);
#[derive(Clone, Copy)]
struct Cap24(u32);
#[derive(Clone, Copy)]
struct Cap25(u32);
#[derive(Clone, Copy)]
struct Cap26(u32);
#[derive(Clone, Copy)]
struct Cap27(u32);
#[derive(Clone, Copy)]
struct Cap28(u32);
#[derive(Clone, Copy)]
struct Cap29(u32);
#[derive(Clone, Copy)]
struct Cap30(u32);
#[derive(Clone, Copy)]
struct Cap31(u32);
#[derive(Clone, Copy)]
struct Cap32(u32);
#[derive(Clone, Copy)]
struct Cap33(u32);
#[derive(Clone, Copy)]
struct Cap34(u32);
#[derive(Clone, Copy)]
struct Cap35(u32);
#[derive(Clone, Copy)]
struct Cap36(u32);
#[derive(Clone, Copy)]
struct Cap37(u32);
#[derive(Clone, Copy)]
struct Cap38(u32);
#[derive(Clone, Copy)]
struct Cap39(u32);
#[derive(Clone, Copy)]
struct Cap40(u32);
#[derive(Clone, Copy)]
struct Cap41(u32);
#[derive(Clone, Copy)]
struct Cap42(u32);
#[derive(Clone, Copy)]
struct Cap43(u32);
#[derive(Clone, Copy)]
struct Cap44(u32);
#[derive(Clone, Copy)]
struct Cap45(u32);
#[derive(Clone, Copy)]
struct Cap46(u32);
#[derive(Clone, Copy)]
struct Cap47(u32);
#[derive(Clone, Copy)]
struct Cap48(u32);
#[derive(Clone, Copy)]
struct Cap49(u32);
#[derive(Clone, Copy)]
struct Cap50(u32);
#[derive(Clone, Copy)]
struct Cap51(u32);
#[derive(Clone, Copy)]
struct Cap52(u32);
#[derive(Clone, Copy)]
struct Cap53(u32);
#[derive(Clone, Copy)]
struct Cap54(u32);
#[derive(Clone, Copy)]
struct Cap55(u32);
#[derive(Clone, Copy)]
struct Cap56(u32);
#[derive(Clone, Copy)]
struct Cap57(u32);
#[derive(Clone, Copy)]
struct Cap58(u32);
#[derive(Clone, Copy)]
struct Cap59(u32);
#[derive(Clone, Copy)]
struct Cap60(u32);
#[derive(Clone, Copy)]
struct Cap61(u32);
#[derive(Clone, Copy)]
struct Cap62(u32);
#[derive(Clone, Copy)]
struct Cap63(u32);

fn bench_env_n(c: &mut Criterion, name: &str, setup: impl Fn() -> DynEnv, get: fn(&DynEnv) -> u32) {
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
      let mut env = DynEnv::new();
      env.insert::<Cap0>(Cap0(0));
      env.insert::<Cap1>(Cap1(0));
      env.insert::<Cap2>(Cap2(0));
      env.insert::<Cap3>(Cap3(0));
      env
    },
    |e| e.try_get::<Cap3>().expect("capability was inserted").0,
  );

  bench_env_n(
    c,
    "env_get_n16",
    || {
      let mut env = DynEnv::new();
      env.insert::<Cap0>(Cap0(0));
      env.insert::<Cap1>(Cap1(0));
      env.insert::<Cap2>(Cap2(0));
      env.insert::<Cap3>(Cap3(0));
      env.insert::<Cap4>(Cap4(0));
      env.insert::<Cap5>(Cap5(0));
      env.insert::<Cap6>(Cap6(0));
      env.insert::<Cap7>(Cap7(0));
      env.insert::<Cap8>(Cap8(0));
      env.insert::<Cap9>(Cap9(0));
      env.insert::<Cap10>(Cap10(0));
      env.insert::<Cap11>(Cap11(0));
      env.insert::<Cap12>(Cap12(0));
      env.insert::<Cap13>(Cap13(0));
      env.insert::<Cap14>(Cap14(0));
      env.insert::<Cap15>(Cap15(0));
      env
    },
    |e| e.try_get::<Cap15>().expect("capability was inserted").0,
  );

  bench_env_n(
    c,
    "env_get_n64",
    || {
      let mut env = DynEnv::new();
      env.insert::<Cap0>(Cap0(0));
      env.insert::<Cap1>(Cap1(0));
      env.insert::<Cap2>(Cap2(0));
      env.insert::<Cap3>(Cap3(0));
      env.insert::<Cap4>(Cap4(0));
      env.insert::<Cap5>(Cap5(0));
      env.insert::<Cap6>(Cap6(0));
      env.insert::<Cap7>(Cap7(0));
      env.insert::<Cap8>(Cap8(0));
      env.insert::<Cap9>(Cap9(0));
      env.insert::<Cap10>(Cap10(0));
      env.insert::<Cap11>(Cap11(0));
      env.insert::<Cap12>(Cap12(0));
      env.insert::<Cap13>(Cap13(0));
      env.insert::<Cap14>(Cap14(0));
      env.insert::<Cap15>(Cap15(0));
      env.insert::<Cap16>(Cap16(0));
      env.insert::<Cap17>(Cap17(0));
      env.insert::<Cap18>(Cap18(0));
      env.insert::<Cap19>(Cap19(0));
      env.insert::<Cap20>(Cap20(0));
      env.insert::<Cap21>(Cap21(0));
      env.insert::<Cap22>(Cap22(0));
      env.insert::<Cap23>(Cap23(0));
      env.insert::<Cap24>(Cap24(0));
      env.insert::<Cap25>(Cap25(0));
      env.insert::<Cap26>(Cap26(0));
      env.insert::<Cap27>(Cap27(0));
      env.insert::<Cap28>(Cap28(0));
      env.insert::<Cap29>(Cap29(0));
      env.insert::<Cap30>(Cap30(0));
      env.insert::<Cap31>(Cap31(0));
      env.insert::<Cap32>(Cap32(0));
      env.insert::<Cap33>(Cap33(0));
      env.insert::<Cap34>(Cap34(0));
      env.insert::<Cap35>(Cap35(0));
      env.insert::<Cap36>(Cap36(0));
      env.insert::<Cap37>(Cap37(0));
      env.insert::<Cap38>(Cap38(0));
      env.insert::<Cap39>(Cap39(0));
      env.insert::<Cap40>(Cap40(0));
      env.insert::<Cap41>(Cap41(0));
      env.insert::<Cap42>(Cap42(0));
      env.insert::<Cap43>(Cap43(0));
      env.insert::<Cap44>(Cap44(0));
      env.insert::<Cap45>(Cap45(0));
      env.insert::<Cap46>(Cap46(0));
      env.insert::<Cap47>(Cap47(0));
      env.insert::<Cap48>(Cap48(0));
      env.insert::<Cap49>(Cap49(0));
      env.insert::<Cap50>(Cap50(0));
      env.insert::<Cap51>(Cap51(0));
      env.insert::<Cap52>(Cap52(0));
      env.insert::<Cap53>(Cap53(0));
      env.insert::<Cap54>(Cap54(0));
      env.insert::<Cap55>(Cap55(0));
      env.insert::<Cap56>(Cap56(0));
      env.insert::<Cap57>(Cap57(0));
      env.insert::<Cap58>(Cap58(0));
      env.insert::<Cap59>(Cap59(0));
      env.insert::<Cap60>(Cap60(0));
      env.insert::<Cap61>(Cap61(0));
      env.insert::<Cap62>(Cap62(0));
      env.insert::<Cap63>(Cap63(0));
      env
    },
    |e| e.try_get::<Cap63>().expect("capability was inserted").0,
  );
}

criterion_group!(benches, bench_env_lookup);
criterion_main!(benches);
