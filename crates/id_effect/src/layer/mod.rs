//! **Stratum 5 — Layers & Dependency Injection**
//!
//! Compositional construction of service environments, built from Strata 0–4.
//!
//! | Submodule | Provides | Depends on |
//! |-----------|----------|------------|
//! | [`factory`] | [`Layer`], [`LayerExt`], [`LayerFn`], [`LayerFnFrom`], [`LayerFrom`], [`Stack`], [`StackThen`], constructors | Stratum 0 (`func::compose`, `func::pipe1`), Stratum 2 (`kernel::Effect`), Stratum 3 (`context::{Cons, Nil}`), Stratum 6 (`runtime::run_blocking`) |
//! | [`graph`] | [`LayerGraph`], [`LayerNode`], [`LayerPlan`], [`LayerPlannerError`], [`LayerDiagnostic`] | Stratum 0 (`hash_map`, `mutable_list`), Stratum 6 (`runtime::run_blocking`), Stratum 12 (`stm::TRef`, optional) |
//!
//! ## Design
//!
//! A [`Layer`] is a recipe for constructing a single heterogeneous cell; multiple layers are
//! combined into a typed `Context` via [`Stack`] / [`StackThen`].  The relationship to the core
//! effect type is:
//!
//! ```text
//! Layer[Out, Err] ≅ Effect[Out, Err, ()]
//! ```
//!
//! [`LayerGraph`] adds a service-name–based planner that computes a valid topological build order
//! from `requires` / `provides` annotations on [`LayerNode`] values.
//!
//! ## Public API
//!
//! Re-exported at the crate root: [`Layer`], [`LayerExt`], [`LayerEffect`], [`LayerFn`],
//! [`LayerFnFrom`], [`LayerFrom`], [`Stack`], [`StackThen`], [`merge_all`],
//! [`LayerGraph`], [`LayerNode`], [`LayerPlan`], [`LayerPlannerError`], [`LayerDiagnostic`].

pub mod factory;
pub mod graph;
pub mod service;

pub use factory::{
  Layer, LayerEffect, LayerExt, LayerFn, LayerFnFrom, LayerFrom, Stack, StackThen, effect, fail,
  from_fn, merge_all, succeed,
};
pub use graph::{LayerDiagnostic, LayerGraph, LayerNode, LayerPlan, LayerPlannerError};
pub use service::{
  Service, ServiceEnv, layer_service, layer_service_env, provide_service, service, service_env,
};
