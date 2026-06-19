#![allow(dead_code, clippy::new_ret_no_self)]

//! Targeted integration tests to exercise uncovered capability and runtime paths.

use id_effect::capability::{CapabilityId, RunError, build_env};
use id_effect::{Effect, caps, effect, provide, run, run_with};

#[::id_effect::capability(u32)]
struct GateAlpha;

#[derive(::id_effect::ProviderSpecDerive)]
#[provides(GateAlphaKey)]
struct GateAlphaLive;

impl GateAlphaLive {
  fn new() -> u32 {
    42
  }
}

#[test]
fn capability_runtime_entrypoints() {
  let id = CapabilityId::of::<GateAlphaKey>();
  let with_v = id.with_variant(Some("primary"));
  assert_eq!(id.variant(), None);
  assert_eq!(with_v.variant(), Some("primary"));
  assert!(format!("{id:?}").contains("CapabilityId"));

  let app: Effect<u32, (), ()> = effect!(42);
  assert_eq!(run(app).unwrap(), 42);

  let env = build_env([provide!(GateAlphaLive)]).expect("build_env");
  assert_eq!(*env.get::<GateAlphaKey>(), 42);

  let ok_app: Effect<u32, (), caps!(GateAlphaKey)> =
    effect!(|r: &mut caps!(GateAlphaKey)| { *~GateAlphaKey });
  let fail_app: Effect<u32, (), caps!(GateAlphaKey)> =
    effect!(|r: &mut caps!(GateAlphaKey)| { *~GateAlphaKey });
  assert_eq!(run_with([provide!(GateAlphaLive)], ok_app).unwrap(), 42);
  assert!(matches!(
    run_with([], fail_app),
    Err(RunError::Capability(_))
  ));
}
