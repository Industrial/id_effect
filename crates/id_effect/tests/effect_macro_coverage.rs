#![allow(dead_code, clippy::type_complexity)]

//! Compile-time coverage for `effect!` / implicit `|r|` / `~Key` macro paths.

use id_effect::{Effect, caps, effect, provide, run_with};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Alpha(u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Beta(u32);
#[derive(Clone, Debug, PartialEq, Eq)]
struct Gamma(String);

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Alpha, with = || Alpha(1))]
struct AlphaLive;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Beta, with = || Beta(2))]
struct BetaLive;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(Gamma, with = || Gamma("g".into()))]
struct GammaLive;

#[test]
fn effect_macro_paths_integration() {
  let single: Effect<u32, (), caps!(Alpha)> = effect!(|r| {
    let a = ~Alpha;
    a.0
  });
  assert_eq!(run_with([provide!(AlphaLive)], single).unwrap(), 1);

  let multi: Effect<String, (), caps!(Alpha, Beta, Gamma)> = effect!(|r| {
    let _a = ~Alpha;
    let _b = ~Beta;
    let g = ~Gamma;
    g.0.clone()
  });
  assert_eq!(
    run_with(
      [provide!(AlphaLive), provide!(BetaLive), provide!(GammaLive)],
      multi
    )
    .unwrap(),
    "g"
  );

  let explicit: Effect<u32, (), caps!(Alpha)> = effect!(|r: &mut caps!(Alpha)| { (~Alpha).0 });
  assert_eq!(run_with([provide!(AlphaLive)], explicit).unwrap(), 1);

  let require_alias: Effect<u32, (), caps!(Alpha)> = effect!(|r| {
    let a = require!(Alpha);
    a.0
  });
  assert_eq!(run_with([provide!(AlphaLive)], require_alias).unwrap(), 1);
}
