//! Live hardware feedback for Compute Fabric.

use std::sync::{Arc, Mutex, mpsc};
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
/// Never blocks the calling thread to sample:
///
/// - Construction is free; nothing is sampled until the first [`TelemetryEngine::snapshot`].
/// - The first snapshot starts the priming sample (creating the [`sysinfo::System`], which
///   takes ~350 ms on Windows) on a background thread and returns at once with the neutral
///   reading ([`TelemetrySnapshot::NEUTRAL`]: `cpu_pct == 0.0`, `mem_pct == 0.0`). Snapshots
///   keep returning that neutral reading until the priming sample lands (see
///   [`SysinfoTelemetry::is_primed`]).
/// - The first snapshot after priming reports real memory but a neutral CPU reading: CPU usage
///   needs two refreshes spaced by [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`], and this engine
///   never sleeps to get them.
/// - Later snapshots refresh only once at least that interval has elapsed since the previous
///   refresh (CPU usage then covers the real elapsed time); earlier calls reuse the cached
///   reading.
#[derive(Debug, Default)]
pub struct SysinfoTelemetry {
  state: Mutex<SysinfoPhase>,
}

#[derive(Debug, Default)]
enum SysinfoPhase {
  /// No sample requested yet.
  #[default]
  Idle,
  /// The priming sample is being taken on a background thread.
  Priming(mpsc::Receiver<sysinfo::System>),
  /// Primed: refreshes are cheap and happen on the calling thread.
  Ready(SysinfoState),
}

#[derive(Debug)]
struct SysinfoState {
  sys: sysinfo::System,
  last_refresh: Instant,
  last: TelemetrySnapshot,
}

impl TelemetrySnapshot {
  /// Reading reported while no real sample is available: zero CPU and zero memory use.
  pub const NEUTRAL: Self = Self {
    cpu_pct: 0.0,
    mem_pct: 0.0,
  };
}

impl SysinfoTelemetry {
  /// Telemetry engine that starts sampling (in the background) on the first
  /// [`TelemetryEngine::snapshot`].
  pub fn new() -> Self {
    Self::default()
  }

  /// Whether sampling has started, i.e. [`TelemetryEngine::snapshot`] was called at least once.
  /// The priming sample may still be in flight; see [`SysinfoTelemetry::is_primed`].
  pub fn is_initialized(&self) -> bool {
    !matches!(*self.lock(), SysinfoPhase::Idle)
  }

  /// Whether the priming sample has landed, so snapshots report real readings. Becomes `true`
  /// on the first [`TelemetryEngine::snapshot`] after the background sample completes.
  pub fn is_primed(&self) -> bool {
    matches!(*self.lock(), SysinfoPhase::Ready(_))
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, SysinfoPhase> {
    self
      .state
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// Creating the System refreshes memory and records the CPU baseline; no sleep, but slow
  /// on Windows, so it runs off the calling thread.
  fn prime() -> sysinfo::System {
    use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
    System::new_with_specifics(
      RefreshKind::nothing()
        .with_memory(MemoryRefreshKind::everything())
        .with_cpu(CpuRefreshKind::everything()),
    )
  }

  fn start_priming() -> SysinfoPhase {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
      .name("id-effect-sysinfo-prime".into())
      .spawn(move || {
        // The receiver may be gone if the engine was dropped; nothing to do then.
        let _ = tx.send(Self::prime());
      });
    match spawned {
      Ok(_) => SysinfoPhase::Priming(rx),
      // No thread available: prime inline rather than never sampling.
      Err(_) => SysinfoPhase::Ready(Self::ready(Self::prime())),
    }
  }

  fn ready(sys: sysinfo::System) -> SysinfoState {
    let last = Self::read(&sys, false);
    SysinfoState {
      sys,
      last_refresh: Instant::now(),
      last,
    }
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
    let mut guard = self.lock();
    let phase = &mut *guard;
    match phase {
      SysinfoPhase::Ready(state) => {
        if state.last_refresh.elapsed() >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL {
          state.sys.refresh_memory();
          state.sys.refresh_cpu_usage();
          state.last_refresh = Instant::now();
          state.last = Self::read(&state.sys, true);
        }
        state.last
      }
      SysinfoPhase::Priming(rx) => match rx.try_recv() {
        Ok(sys) => {
          let state = Self::ready(sys);
          let last = state.last;
          *phase = SysinfoPhase::Ready(state);
          last
        }
        Err(mpsc::TryRecvError::Empty) => TelemetrySnapshot::NEUTRAL,
        // The priming thread died (sysinfo panicked): retry next time.
        Err(mpsc::TryRecvError::Disconnected) => {
          *phase = Self::start_priming();
          TelemetrySnapshot::NEUTRAL
        }
      },
      SysinfoPhase::Idle => {
        *phase = Self::start_priming();
        TelemetrySnapshot::NEUTRAL
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

/// Polls `snapshot` until the background priming sample lands; returns the first primed
/// reading (tests only).
#[cfg(test)]
pub(crate) fn wait_primed(engine: &SysinfoTelemetry) -> TelemetrySnapshot {
  let deadline = Instant::now() + std::time::Duration::from_secs(30);
  loop {
    let snap = engine.snapshot();
    if engine.is_primed() {
      return snap;
    }
    assert_eq!(snap, TelemetrySnapshot::NEUTRAL, "unprimed reading");
    assert!(Instant::now() < deadline, "sysinfo never primed");
    std::thread::sleep(std::time::Duration::from_millis(5));
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
    assert!(!engine.is_primed());
    let started = Instant::now();
    let first = engine.snapshot();
    let elapsed = started.elapsed();
    // The priming sample (~350 ms on Windows) runs off the calling thread.
    assert!(
      elapsed < std::time::Duration::from_millis(50),
      "first snapshot blocked for {elapsed:?}"
    );
    assert!(engine.is_initialized());
    // Until the priming sample lands, the reading is neutral.
    assert_eq!(first, TelemetrySnapshot::NEUTRAL);
    let primed = wait_primed(&engine);
    assert!(primed.mem_pct > 0.0 && primed.mem_pct <= 1.0);
    // No CPU baseline yet: neutral CPU reading.
    assert_eq!(primed.cpu_pct, 0.0);
    // Within MINIMUM_CPU_UPDATE_INTERVAL the cached reading is reused.
    assert_eq!(engine.snapshot(), primed);
  }

  #[test]
  fn sysinfo_telemetry_refreshes_cpu_after_the_minimum_interval() {
    let engine = SysinfoTelemetry::new();
    wait_primed(&engine);
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    let snap = engine.snapshot();
    assert!((0.0..=1.0).contains(&snap.cpu_pct), "cpu {snap:?}");
    assert!(snap.mem_pct > 0.0 && snap.mem_pct <= 1.0);
  }

  #[test]
  fn sysinfo_telemetry_restarts_priming_when_the_sampler_dies() {
    let engine = SysinfoTelemetry::new();
    let (tx, rx) = mpsc::channel();
    drop(tx);
    *engine.lock() = SysinfoPhase::Priming(rx);
    assert_eq!(engine.snapshot(), TelemetrySnapshot::NEUTRAL);
    assert!(engine.is_initialized());
    wait_primed(&engine);
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
