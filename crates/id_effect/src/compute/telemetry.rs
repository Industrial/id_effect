//! Live hardware feedback for Compute Fabric.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::policy::ResourcePolicy;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TelemetrySnapshot {
  pub cpu_pct: f32,
  pub mem_pct: f32,
}

impl TelemetrySnapshot {
  pub fn satisfies(&self, policy: &ResourcePolicy) -> bool {
    policy.cpu.mode.is_within(self.cpu_pct) && policy.memory.mode.is_within(self.mem_pct)
  }

  pub fn min_headroom(&self, policy: &ResourcePolicy) -> f32 {
    policy
      .cpu
      .mode
      .headroom(self.cpu_pct)
      .min(policy.memory.mode.headroom(self.mem_pct))
  }
}

pub trait TelemetryEngine: Send + Sync {
  fn snapshot(&self) -> TelemetrySnapshot;
}

/// Host telemetry backed by [`sysinfo`].
///
/// Never blocks to sample:
///
/// - Construction is free; the underlying [`sysinfo::System`] is created on the first
///   [`TelemetryEngine::snapshot`].
/// - That first snapshot records a CPU baseline and reports real memory but a neutral CPU
///   reading (`cpu_pct == 0.0`): CPU usage needs two refreshes spaced by
///   [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`], and this engine never sleeps to get them.
/// - Later snapshots refresh only once at least that interval has elapsed since the previous
///   refresh (CPU usage then covers the real elapsed time); earlier calls reuse the cached
///   reading.
#[derive(Debug, Default)]
pub struct SysinfoTelemetry {
  state: Mutex<Option<SysinfoState>>,
}

#[derive(Debug)]
struct SysinfoState {
  sys: sysinfo::System,
  last_refresh: Instant,
  last: TelemetrySnapshot,
}

impl SysinfoTelemetry {
  /// Telemetry engine that samples lazily on first [`TelemetryEngine::snapshot`].
  pub fn new() -> Self {
    Self::default()
  }

  /// Whether the underlying [`sysinfo::System`] has been created (i.e. a sample was taken).
  pub fn is_initialized(&self) -> bool {
    self
      .state
      .lock()
      .map(|guard| guard.is_some())
      .unwrap_or(false)
  }

  fn read(sys: &sysinfo::System, cpu_known: bool) -> TelemetrySnapshot {
    let mem_pct = if sys.total_memory() > 0 {
      sys.used_memory() as f32 / sys.total_memory() as f32
    } else {
      0.0
    };
    let cpus = sys.cpus();
    let cpu_pct = if !cpu_known || cpus.is_empty() {
      0.0
    } else {
      cpus.iter().map(|c| c.cpu_usage()).sum::<f32>() / (cpus.len() as f32 * 100.0)
    };
    TelemetrySnapshot { cpu_pct, mem_pct }
  }
}

impl TelemetryEngine for SysinfoTelemetry {
  fn snapshot(&self) -> TelemetrySnapshot {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};

    let mut guard = self
      .state
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_mut() {
      Some(state) => {
        if state.last_refresh.elapsed() >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL {
          state.sys.refresh_memory();
          state.sys.refresh_cpu_usage();
          state.last_refresh = Instant::now();
          state.last = Self::read(&state.sys, true);
        }
        state.last
      }
      None => {
        // Creating the System refreshes memory and records the CPU baseline; no sleep.
        let sys = System::new_with_specifics(
          RefreshKind::nothing()
            .with_memory(MemoryRefreshKind::everything())
            .with_cpu(sysinfo::CpuRefreshKind::everything()),
        );
        let last = Self::read(&sys, false);
        *guard = Some(SysinfoState {
          sys,
          last_refresh: Instant::now(),
          last,
        });
        last
      }
    }
  }
}

#[derive(Debug)]
pub struct MockTelemetry {
  inner: Mutex<TelemetrySnapshot>,
}

impl MockTelemetry {
  pub fn new(cpu_pct: f32, mem_pct: f32) -> Arc<Self> {
    Arc::new(Self {
      inner: Mutex::new(TelemetrySnapshot { cpu_pct, mem_pct }),
    })
  }

  pub fn set(&self, cpu_pct: f32, mem_pct: f32) {
    *self.inner.lock().expect("mock telemetry lock") = TelemetrySnapshot { cpu_pct, mem_pct };
  }
}

impl TelemetryEngine for MockTelemetry {
  fn snapshot(&self) -> TelemetrySnapshot {
    *self.inner.lock().expect("mock telemetry lock")
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::compute::policy::{MetricMode, MetricPolicy, RebalanceStrategy};

  #[test]
  fn mock_telemetry_returns_configured_values() {
    let engine = MockTelemetry::new(0.4, 0.61);
    let snap = engine.snapshot();
    assert!((snap.cpu_pct - 0.4).abs() < 0.001);
    assert!((snap.mem_pct - 0.61).abs() < 0.001);
  }

  #[test]
  fn sysinfo_telemetry_is_lazy_until_first_snapshot() {
    let engine = SysinfoTelemetry::new();
    assert!(!engine.is_initialized());
    let started = Instant::now();
    let first = engine.snapshot();
    let elapsed = started.elapsed();
    // Windows' first system sample takes ~350 ms (CI, 0.12); moving it off the calling thread
    // is a 0.13 follow-up. Elsewhere it stays under 50 ms.
    let bound = if cfg!(windows) {
      std::time::Duration::from_secs(1)
    } else {
      std::time::Duration::from_millis(50)
    };
    assert!(elapsed < bound, "first snapshot blocked for {elapsed:?}");
    assert!(engine.is_initialized());
    assert!(first.mem_pct >= 0.0);
    // No CPU baseline yet: neutral CPU reading.
    assert_eq!(first.cpu_pct, 0.0);
    // Within MINIMUM_CPU_UPDATE_INTERVAL the cached reading is reused.
    assert_eq!(engine.snapshot(), first);
  }

  #[test]
  fn snapshot_satisfies_policy_under_ceiling() {
    let snap = TelemetrySnapshot {
      cpu_pct: 0.4,
      mem_pct: 0.61,
    };
    let policy = crate::compute::policy::ResourcePolicy {
      memory: MetricPolicy::new(MetricMode::Max { ceiling: 0.85 }),
      cpu: MetricPolicy::new(MetricMode::Max { ceiling: 1.0 }),
      rebalance: RebalanceStrategy::ThrottleAdmission,
    };
    assert!(snap.satisfies(&policy));
    assert!(snap.min_headroom(&policy) > 0.2);
  }

  #[test]
  fn snapshot_rejects_breach_and_mock_set_updates() {
    let engine = MockTelemetry::new(0.4, 0.61);
    engine.set(0.95, 0.95);
    let snap = engine.snapshot();
    let policy = crate::compute::policy::ResourcePolicy::memory_cap_max_cpu(0.85);
    assert!(!snap.satisfies(&policy));
    assert_eq!(snap.min_headroom(&policy), 0.0);
  }
}
