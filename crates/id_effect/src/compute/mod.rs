//! Compute Fabric
#![allow(missing_docs)]
// stratum 5.5 — expand rustdoc in book pass — resource-aware execution substrate (Stratum 5.5).
//!
//! Two layers:
//!
//! - [`Fabric`] (always built): the fiber admission scheduler of the kernel. `Concurrency::Inherit`
//!   children of `fork_all` (and so of `all`, `for_each_par`, `zip_par`, …) await one of its
//!   permits; see the `kernel_fabric` docs on [`Fabric`].
//! - The resource-aware substrate (feature `compute`): the [`FabricGovernor`] compares live
//!   telemetry against [`ResourcePolicy`] and adjusts admission, pool sizing and placement
//!   ([`ComputeFabric`], [`AdmissionController`], adaptive Rayon parallelism). It governs
//!   resources; it does not restart failed work (that is the `Supervisor`'s job).
//!   [`Fabric::from_compute`] puts a [`ComputeFabric`]'s admission in front of fibers.

mod kernel_fabric;

pub use kernel_fabric::*;

#[cfg(feature = "compute")]
mod adaptive;
#[cfg(feature = "compute")]
mod admission;
#[cfg(feature = "compute")]
mod dispatch;
#[cfg(feature = "compute")]
mod fabric;
#[cfg(feature = "compute")]
mod fiber_pool;
#[cfg(feature = "compute")]
mod governor;
#[cfg(feature = "compute")]
mod policy;
#[cfg(feature = "compute")]
mod rayon_pool;
#[cfg(feature = "compute")]
mod spread;
#[cfg(feature = "compute")]
mod telemetry;

#[cfg(feature = "compute")]
pub use adaptive::{
  AdaptiveContext, current_adaptive_context, ensure_default_fabric, ensure_run_context,
  install_fabric, installed_fabric, is_fabric_installed, refresh_adaptive_context,
  uninstall_fabric,
};
#[cfg(feature = "compute")]
pub use admission::{AdmissionController, AdmissionGuard, Admit};
#[cfg(feature = "compute")]
pub use dispatch::{parallel_if_profitable, should_parallelize};
#[cfg(feature = "compute")]
pub use fabric::ComputeFabric;
#[cfg(feature = "compute")]
pub use fiber_pool::FiberPool;
#[cfg(feature = "compute")]
pub use governor::{FabricGovernor, FabricJobSpec};
#[cfg(feature = "compute")]
pub use policy::{MetricMode, MetricPolicy, RebalanceStrategy, ResourcePolicy, WorkProfile};
#[cfg(feature = "compute")]
pub use rayon_pool::{configure_rayon_threads, install_parallel};
#[cfg(feature = "compute")]
pub use spread::CpuSpreadBucket;
#[cfg(feature = "compute")]
pub use telemetry::{MockTelemetry, SysinfoTelemetry, TelemetryEngine, TelemetrySnapshot};
