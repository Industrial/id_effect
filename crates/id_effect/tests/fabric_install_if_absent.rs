//! Regression: entry points must keep a pre-installed [`ComputeFabric`] instead of replacing it.
//!
//! The fabric slot is process-global, so every scenario lives in one `#[test]` to stay
//! deterministic under both `cargo test` (shared process) and nextest.

use std::sync::Arc;

use id_effect::compute::{
  ComputeFabric, MockTelemetry, ResourcePolicy, SysinfoTelemetry, ensure_default_fabric,
  install_fabric, installed_fabric, is_fabric_installed, uninstall_fabric,
};
use id_effect::{Effect, ProviderBox, Runtime, ThreadSleepRuntime, from_async, run, run_with};

fn assert_installed(expected: &Arc<ComputeFabric<SysinfoTelemetry>>, at: &str) {
  let current = installed_fabric::<SysinfoTelemetry>().expect("a sysinfo fabric is installed");
  assert!(
    Arc::ptr_eq(&current, expected),
    "{at}: custom fabric was replaced"
  );
  assert_eq!(current.policy(), &ResourcePolicy::memory_cap_max_cpu(0.42));
}

/// Effect that checks, while running, that `expected` is still the installed fabric.
fn probe(expected: Arc<ComputeFabric<SysinfoTelemetry>>, at: &'static str) -> Effect<u8, (), ()> {
  from_async(move |_env| async move {
    assert_installed(&expected, at);
    Ok::<u8, ()>(1)
  })
}

#[test]
fn preinstalled_fabric_survives_every_entry_point() {
  uninstall_fabric();
  assert!(!is_fabric_installed());

  let custom = Arc::new(ComputeFabric::memory_cap_max_cpu(0.42));
  install_fabric(Arc::clone(&custom));

  // run
  assert_eq!(
    run(probe(Arc::clone(&custom), "run (during)")).ok(),
    Some(1)
  );
  assert_installed(&custom, "run (after)");

  // run_with
  let out = run_with(
    Vec::<ProviderBox>::new(),
    probe(Arc::clone(&custom), "run_with (during)"),
  );
  assert_eq!(out.ok(), Some(1));
  assert_installed(&custom, "run_with (after)");

  // ThreadSleepRuntime::default reuses the installed fabric for its pool.
  let rt = ThreadSleepRuntime::default();
  assert!(std::ptr::eq(rt.fabric(), Arc::as_ptr(&custom)));
  let expected = Arc::clone(&custom);
  let handle = rt.spawn_with(move || (probe(expected, "ThreadSleepRuntime (during)"), ()));
  assert_eq!(pollster::block_on(handle.join()), Ok(1));
  assert_installed(&custom, "ThreadSleepRuntime (after)");

  // ensure_default_fabric itself is install-if-absent.
  assert!(Arc::ptr_eq(&ensure_default_fabric(), &custom));
  assert_installed(&custom, "ensure_default_fabric");

  // A fabric with a non-sysinfo engine is also kept; runtimes get a private pool.
  let mock = Arc::new(ComputeFabric::with_mock(
    ResourcePolicy::memory_cap_max_cpu(0.33),
    0.1,
    0.2,
  ));
  install_fabric(Arc::clone(&mock));
  assert_eq!(
    run(from_async(|_env| async { Ok::<u8, ()>(2) })).ok(),
    Some(2)
  );
  let rt = ThreadSleepRuntime::default();
  assert!(!std::ptr::eq(rt.fabric(), Arc::as_ptr(&custom)));
  let still = installed_fabric::<MockTelemetry>().expect("mock fabric still installed");
  assert!(Arc::ptr_eq(&still, &mock));
  assert!(installed_fabric::<SysinfoTelemetry>().is_none());

  // With nothing installed, the default fabric is installed and does not sample telemetry.
  assert!(uninstall_fabric());
  assert_eq!(
    run(from_async(|_env| async { Ok::<u8, ()>(3) })).ok(),
    Some(3)
  );
  let default = installed_fabric::<SysinfoTelemetry>().expect("default fabric installed");
  assert_eq!(default.policy(), &ResourcePolicy::memory_cap_max_cpu(1.0));
  assert!(!default.telemetry().is_initialized());
  uninstall_fabric();
}
