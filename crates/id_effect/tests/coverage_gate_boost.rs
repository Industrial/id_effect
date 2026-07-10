#![allow(dead_code, clippy::new_ret_no_self)]

//! Targeted integration tests to exercise uncovered capability and runtime paths.

use id_effect::capability::{CapabilityId, RunError, build_env};
use id_effect::{Cap, Effect, caps, effect, provide, run, run_with};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GateAlpha(u32);

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(GateAlpha)]
struct GateAlphaLive;

impl GateAlphaLive {
  fn new() -> GateAlpha {
    GateAlpha(42)
  }
}

#[test]
fn capability_runtime_entrypoints() {
  let id = CapabilityId::of::<GateAlpha>();
  let with_v = id.with_variant(Some("primary"));
  assert_eq!(id.variant(), None);
  assert_eq!(with_v.variant(), Some("primary"));
  assert!(format!("{id:?}").contains("CapabilityId"));

  let app: Effect<u32, (), ()> = effect!(42);
  assert_eq!(run(app).unwrap(), 42);

  let env = build_env([provide!(GateAlphaLive)]).expect("build_env");
  assert_eq!(env.get::<Cap<GateAlpha>>().0, 42);

  let ok_app: Effect<u32, (), caps!(GateAlpha)> =
    effect!(|r: &mut caps!(GateAlpha)| { (~GateAlpha).0 });
  let fail_app: Effect<u32, (), caps!(GateAlpha)> =
    effect!(|r: &mut caps!(GateAlpha)| { (~GateAlpha).0 });
  assert_eq!(run_with([provide!(GateAlphaLive)], ok_app).unwrap(), 42);
  assert!(matches!(
    run_with([], fail_app),
    Err(RunError::Capability(_))
  ));
  assert!(Cap::<GateAlpha>::slot_name().contains("GateAlpha"));
}
