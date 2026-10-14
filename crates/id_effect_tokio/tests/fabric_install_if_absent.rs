//! Regression: `TokioRuntime` constructors must keep a pre-installed [`ComputeFabric`].
//!
//! The fabric slot is process-global, so all scenarios live in one `#[test]`.

use std::sync::Arc;

use id_effect::compute::{
  ComputeFabric, ResourcePolicy, SysinfoTelemetry, install_fabric, installed_fabric,
  uninstall_fabric,
};
use id_effect::{Effect, from_async, run_fork};
use id_effect_tokio::TokioRuntime;

fn assert_installed(expected: &Arc<ComputeFabric<SysinfoTelemetry>>, at: &str) {
  let current = installed_fabric::<SysinfoTelemetry>().expect("a sysinfo fabric is installed");
  assert!(
    Arc::ptr_eq(&current, expected),
    "{at}: custom fabric was replaced"
  );
  assert_eq!(current.policy(), &ResourcePolicy::memory_cap_max_cpu(0.42));
}

#[test]
fn tokio_default_runtime_keeps_preinstalled_fabric() {
  uninstall_fabric();
  let custom = Arc::new(ComputeFabric::memory_cap_max_cpu(0.42));
  install_fabric(Arc::clone(&custom));

  let rt = TokioRuntime::new_current_thread().expect("tokio runtime should build");
  assert!(std::ptr::eq(rt.fabric(), Arc::as_ptr(&custom)));
  assert_installed(&custom, "new_current_thread");

  let from_handle = TokioRuntime::from_handle(rt.handle());
  assert!(std::ptr::eq(from_handle.fabric(), Arc::as_ptr(&custom)));
  assert_installed(&custom, "from_handle");

  let expected = Arc::clone(&custom);
  let out = rt.block_on(async {
    let current = TokioRuntime::current().expect("inside tokio context");
    assert!(std::ptr::eq(current.fabric(), Arc::as_ptr(&custom)));
    let h = run_fork(&rt, move || {
      let eff: Effect<u8, (), ()> = from_async(move |_env| async move {
        assert_installed(&expected, "TokioRuntime fiber (during)");
        Ok::<u8, ()>(5)
      });
      (eff, ())
    });
    h.join().await
  });
  assert_eq!(out, Ok(5));
  assert_installed(&custom, "TokioRuntime (after)");
  uninstall_fabric();
}
