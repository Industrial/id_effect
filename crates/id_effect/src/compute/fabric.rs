//! Compute Fabric — installed at run boundaries.

use std::sync::Arc;

use super::admission::AdmissionController;
use super::fiber_pool::FiberPool;
use super::policy::ResourcePolicy;
use super::supervisor::ComputeSupervisor;
use super::telemetry::{MockTelemetry, SysinfoTelemetry, TelemetryEngine};

/// Shared Compute Fabric state for a program run.
#[derive(Clone, Debug)]
pub struct ComputeFabric<E: TelemetryEngine = SysinfoTelemetry> {
  policy: ResourcePolicy,
  pool: Arc<FiberPool>,
  admission: Arc<AdmissionController>,
  supervisor: Arc<ComputeSupervisor<E>>,
  telemetry: Arc<E>,
}

impl ComputeFabric<SysinfoTelemetry> {
  pub fn with_policy(policy: ResourcePolicy) -> Self {
    let pool = Arc::new(FiberPool::new(FiberPool::default_size()));
    let admission = Arc::new(AdmissionController::default());
    let telemetry = Arc::new(SysinfoTelemetry);
    let supervisor = Arc::new(ComputeSupervisor::new(
      policy.clone(),
      Arc::clone(&telemetry),
      Arc::clone(&admission),
    ));
    Self {
      policy,
      pool,
      admission,
      supervisor,
      telemetry,
    }
  }

  pub fn memory_cap_max_cpu(mem_ceiling: f32) -> Self {
    Self::with_policy(ResourcePolicy::memory_cap_max_cpu(mem_ceiling))
  }
}

impl<E: TelemetryEngine + 'static> ComputeFabric<E> {
  pub fn policy(&self) -> &ResourcePolicy {
    &self.policy
  }

  pub fn pool(&self) -> &FiberPool {
    &self.pool
  }

  pub fn admission(&self) -> &AdmissionController {
    &self.admission
  }

  pub fn supervisor(&self) -> &ComputeSupervisor<E> {
    &self.supervisor
  }

  pub fn tick(&self) -> super::telemetry::TelemetrySnapshot {
    self.supervisor.tick()
  }

  /// Install this fabric as the process-wide adaptive context source.
  pub fn install(self: &Arc<Self>) {
    super::adaptive::install_fabric(Arc::clone(self));
  }

  pub fn try_admit<F>(&self, f: F) -> bool
  where
    F: FnOnce() + Send + 'static,
  {
    if self.admission.try_acquire() {
      let admission = Arc::clone(&self.admission);
      self.pool.spawn(move || {
        f();
        admission.release();
      });
      true
    } else {
      false
    }
  }

  /// Spawn on the fiber pool after acquiring an admission permit (blocks if throttled).
  pub fn spawn_admitted<F>(&self, f: F)
  where
    F: FnOnce() + Send + 'static,
  {
    self.admission.acquire_blocking();
    let admission = Arc::clone(&self.admission);
    self.pool.spawn(move || {
      f();
      admission.release();
    });
  }
}

impl ComputeFabric<MockTelemetry> {
  /// Update mock telemetry readings (tests and examples).
  pub fn set_readings(&self, cpu_pct: f32, mem_pct: f32) {
    self.telemetry.set(cpu_pct, mem_pct);
  }

  pub fn with_mock(policy: ResourcePolicy, cpu_pct: f32, mem_pct: f32) -> Self {
    let pool = Arc::new(FiberPool::new(FiberPool::default_size()));
    let admission = Arc::new(AdmissionController::default());
    let telemetry = MockTelemetry::new(cpu_pct, mem_pct);
    let supervisor = Arc::new(ComputeSupervisor::new(
      policy.clone(),
      Arc::clone(&telemetry),
      Arc::clone(&admission),
    ));
    Self {
      policy,
      pool,
      admission,
      supervisor,
      telemetry,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::adaptive;
  use crate::compute::policy::RebalanceStrategy;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicBool, Ordering};
  use std::sync::mpsc;
  use std::time::Duration;

  #[test]
  fn memory_cap_factory_wires_supervisor() {
    let fabric = ComputeFabric::memory_cap_max_cpu(0.85);
    assert_eq!(
      fabric.policy().rebalance,
      RebalanceStrategy::ThrottleAdmission
    );
    assert!(fabric.pool().target_size() >= 1);
  }

  #[test]
  fn with_policy_sysinfo_factory() {
    let fabric = ComputeFabric::with_policy(ResourcePolicy::memory_cap_max_cpu(0.7));
    let snap = fabric.tick();
    assert!(snap.mem_pct >= 0.0);
    assert!(
      fabric
        .supervisor()
        .policy()
        .cpu
        .mode
        .is_within(snap.cpu_pct)
    );
  }

  #[test]
  fn mock_fabric_tick_and_set_readings() {
    let fabric = ComputeFabric::with_mock(ResourcePolicy::memory_cap_max_cpu(0.85), 0.4, 0.61);
    fabric.set_readings(0.9, 0.95);
    let snap = fabric.tick();
    assert!(snap.mem_pct > 0.8);
  }

  #[test]
  fn try_admit_runs_when_permit_available() {
    let fabric = ComputeFabric::with_mock(ResourcePolicy::memory_cap_max_cpu(0.85), 0.2, 0.3);
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    assert!(fabric.try_admit(move || flag.store(true, Ordering::SeqCst)));
    std::thread::sleep(Duration::from_millis(50));
    assert!(ran.load(Ordering::SeqCst));
  }

  #[test]
  fn try_admit_returns_false_when_exhausted() {
    let fabric = ComputeFabric::with_mock(ResourcePolicy::memory_cap_max_cpu(0.85), 0.2, 0.3);
    while fabric.admission().try_acquire() {}
    assert!(!fabric.try_admit(|| {}));
  }

  #[test]
  fn spawn_admitted_executes_on_pool() {
    let fabric = ComputeFabric::with_mock(ResourcePolicy::memory_cap_max_cpu(0.85), 0.2, 0.3);
    let (tx, rx) = mpsc::channel();
    fabric.spawn_admitted(move || tx.send(()).unwrap());
    rx.recv_timeout(Duration::from_secs(2)).expect("spawned");
  }

  #[test]
  fn install_registers_adaptive_source() {
    let fabric = Arc::new(ComputeFabric::with_mock(
      ResourcePolicy::memory_cap_max_cpu(0.85),
      0.3,
      0.4,
    ));
    fabric.install();
    adaptive::refresh_adaptive_context();
    let ctx = adaptive::current_adaptive_context();
    assert!(ctx.admission_budget >= 1);
  }
}
