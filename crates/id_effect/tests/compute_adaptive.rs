//! The process-wide Compute Fabric slot: `ensure_default_fabric`, `install_fabric`,
//! `installed_fabric`, `is_fabric_installed` and `uninstall_fabric`.
//!
//! The slot is global, so the whole lifecycle is one test in its own binary: nothing else in
//! this process installs a fabric concurrently.
#![cfg(feature = "compute")]

use std::sync::Arc;

use id_effect::compute::{
  AdaptiveContext, ComputeFabric, MockTelemetry, ResourcePolicy, SysinfoTelemetry,
  current_adaptive_context, ensure_default_fabric, install_fabric, installed_fabric,
  is_fabric_installed, uninstall_fabric,
};

#[test]
fn the_fabric_slot_installs_once_keeps_a_users_fabric_and_uninstalls() {
  // Start empty: uninstalling twice reports what was there.
  uninstall_fabric();
  assert!(!uninstall_fabric(), "nothing left to uninstall");
  assert!(!is_fabric_installed());
  assert!(installed_fabric::<SysinfoTelemetry>().is_none());
  assert_eq!(current_adaptive_context(), AdaptiveContext::standalone());

  // Nothing installed: the default fabric is built and installed.
  let first = ensure_default_fabric();
  assert!(is_fabric_installed());
  let installed = installed_fabric::<SysinfoTelemetry>().expect("the default fabric");
  assert!(Arc::ptr_eq(&first, &installed));
  assert!(
    installed_fabric::<MockTelemetry>().is_none(),
    "typed lookup"
  );

  // A default fabric is installed: the same one comes back.
  let again = ensure_default_fabric();
  assert!(
    Arc::ptr_eq(&first, &again),
    "an installed default is reused"
  );

  // A fabric with another telemetry engine stays installed; the caller gets a fresh,
  // uninstalled default for its own pool.
  let mock = Arc::new(ComputeFabric::with_mock(
    ResourcePolicy::memory_cap_max_cpu(0.85),
    0.2,
    0.3,
  ));
  install_fabric(Arc::clone(&mock));
  let fresh = ensure_default_fabric();
  assert!(!Arc::ptr_eq(&first, &fresh));
  let still = installed_fabric::<MockTelemetry>().expect("the user's fabric stays");
  assert!(Arc::ptr_eq(&mock, &still));
  assert!(installed_fabric::<SysinfoTelemetry>().is_none());
  assert_eq!(
    current_adaptive_context(),
    AdaptiveContext::from_fabric(&mock),
    "the user's fabric drives the adaptive context"
  );

  // Uninstalling falls back to the standalone context.
  assert!(uninstall_fabric());
  assert!(!is_fabric_installed());
  assert_eq!(current_adaptive_context(), AdaptiveContext::standalone());
}
