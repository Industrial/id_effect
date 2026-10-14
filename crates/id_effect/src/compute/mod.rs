//! Compute Fabric
#![allow(missing_docs)]
// stratum 5.5 — expand rustdoc in book pass — resource-aware execution substrate (Stratum 5.5).
//!
//! Every effect passes through Fabric at the runtime boundary. The [`FabricGovernor`] compares
//! live telemetry against [`ResourcePolicy`] and adjusts admission, pool sizing, and placement.
//! It governs resources; it does not restart failed work (see [`crate::concurrency::Supervisor`]).

mod adaptive;
mod admission;
mod dispatch;
mod fabric;
mod fiber_pool;
mod governor;
mod policy;
mod rayon_pool;
mod spread;
mod telemetry;

pub use adaptive::{
  AdaptiveContext, current_adaptive_context, ensure_default_fabric, ensure_run_context,
  install_fabric, installed_fabric, is_fabric_installed, refresh_adaptive_context,
  uninstall_fabric,
};
pub use admission::AdmissionController;
pub use dispatch::{parallel_if_profitable, should_parallelize};
pub use fabric::ComputeFabric;
pub use fiber_pool::FiberPool;
pub use governor::{FabricGovernor, FabricJobSpec};
pub use policy::{MetricMode, MetricPolicy, RebalanceStrategy, ResourcePolicy, WorkProfile};
pub use rayon_pool::{configure_rayon_threads, install_parallel};
pub use spread::CpuSpreadBucket;
pub use telemetry::{MockTelemetry, SysinfoTelemetry, TelemetryEngine, TelemetrySnapshot};

/// Deprecated name of [`FabricGovernor`].
#[deprecated(since = "0.6.0", note = "renamed to FabricGovernor")]
pub type ComputeSupervisor<E> = FabricGovernor<E>;
