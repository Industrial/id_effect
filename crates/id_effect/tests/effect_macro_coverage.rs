#![allow(dead_code, clippy::new_ret_no_self)]

//! Compile-time coverage for `effect!` / implicit `|r|` / `~Key` macro paths.

use id_effect::{Effect, caps, effect, provide, run_with};

#[::id_effect::capability(u32)]
struct Alpha;
#[::id_effect::capability(u32)]
struct Beta;
#[::id_effect::capability(String)]
struct Gamma;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(AlphaKey)]
struct AlphaLive;
impl AlphaLive {
  fn new() -> u32 {
    1
  }
}

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(BetaKey)]
struct BetaLive;
impl BetaLive {
  fn new() -> u32 {
    2
  }
}

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(GammaKey)]
struct GammaLive;
impl GammaLive {
  fn new() -> String {
    "g".into()
  }
}

#[test]
fn effect_macro_paths_integration() {
  let single: Effect<u32, (), caps!(AlphaKey)> = effect!(|r| {
    let a = ~AlphaKey;
    *a
  });
  assert_eq!(run_with([provide!(AlphaLive)], single).unwrap(), 1);

  let multi: Effect<String, (), caps!(AlphaKey, BetaKey, GammaKey)> = effect!(|r| {
    let _a = ~AlphaKey;
    let _b = ~BetaKey;
    let g = ~GammaKey;
    g.clone()
  });
  assert_eq!(
    run_with(
      [provide!(AlphaLive), provide!(BetaLive), provide!(GammaLive)],
      multi
    )
    .unwrap(),
    "g"
  );

  let explicit: Effect<u32, (), caps!(AlphaKey)> =
    effect!(|r: &mut caps!(AlphaKey)| { *~AlphaKey });
  assert_eq!(run_with([provide!(AlphaLive)], explicit).unwrap(), 1);

  let require_alias: Effect<u32, (), caps!(AlphaKey)> = effect!(|r| {
    let a = require!(AlphaKey);
    *a
  });
  assert_eq!(run_with([provide!(AlphaLive)], require_alias).unwrap(), 1);
}
