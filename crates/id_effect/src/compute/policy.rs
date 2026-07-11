//! Declarative resource rules for Compute Fabric.

/// How a single metric (CPU, memory, …) is governed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MetricMode {
  /// Hard ceiling as a fraction in `[0.0, 1.0]`.
  Max { ceiling: f32 },
  /// Steer toward a setpoint (PID-lite in supervisor).
  Target { setpoint: f32 },
  /// Even share per active worker.
  Spread { per_worker: f32 },
  /// No limit on this metric.
  Unlimited,
}

impl MetricMode {
  /// Whether `current` is within policy for this mode.
  #[inline]
  pub fn is_within(self, current: f32) -> bool {
    match self {
      Self::Max { ceiling } => current <= ceiling,
      Self::Target { setpoint } => (current - setpoint).abs() <= 0.05,
      Self::Spread { per_worker } => current <= per_worker * 1.1,
      Self::Unlimited => true,
    }
  }

  /// Headroom before hitting the policy bound (0 when at/over ceiling).
  #[inline]
  pub fn headroom(self, current: f32) -> f32 {
    match self {
      Self::Max { ceiling } => (ceiling - current).max(0.0),
      Self::Target { setpoint } => (setpoint - current).max(0.0),
      Self::Spread { per_worker } => (per_worker - current).max(0.0),
      Self::Unlimited => 1.0,
    }
  }
}

/// Policy for one metric dimension.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricPolicy {
  pub mode: MetricMode,
}

impl MetricPolicy {
  #[inline]
  pub const fn new(mode: MetricMode) -> Self {
    Self { mode }
  }
}

/// What the supervisor does when policy is breached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RebalanceStrategy {
  #[default]
  ThrottleAdmission,
  ShedLoad,
  ScaleOut,
  ScaleIn,
}

/// Declarative CPU and memory rules.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourcePolicy {
  pub cpu: MetricPolicy,
  pub memory: MetricPolicy,
  pub rebalance: RebalanceStrategy,
}

impl ResourcePolicy {
  pub fn memory_cap_max_cpu(mem_ceiling: f32) -> Self {
    Self {
      memory: MetricPolicy::new(MetricMode::Max {
        ceiling: mem_ceiling,
      }),
      cpu: MetricPolicy::new(MetricMode::Max { ceiling: 1.0 }),
      rebalance: RebalanceStrategy::ThrottleAdmission,
    }
  }

  pub fn unlimited_memory_cpu_spread(per_worker: f32) -> Self {
    Self {
      memory: MetricPolicy::new(MetricMode::Unlimited),
      cpu: MetricPolicy::new(MetricMode::Spread { per_worker }),
      rebalance: RebalanceStrategy::ThrottleAdmission,
    }
  }
}

/// Classification hint for placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkProfile {
  CpuIntensive,
  IoBound,
  MemoryHeavy,
  Remote,
  #[default]
  Mixed,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn max_headroom_when_below_ceiling() {
    let mode = MetricMode::Max { ceiling: 0.85 };
    assert!(mode.is_within(0.61));
    assert!((mode.headroom(0.61) - 0.24).abs() < 0.001);
  }

  #[test]
  fn max_breach_when_over_ceiling() {
    let mode = MetricMode::Max { ceiling: 0.85 };
    assert!(!mode.is_within(0.90));
    assert_eq!(mode.headroom(0.90), 0.0);
  }
}
