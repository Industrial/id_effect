//! `Effect<A, E = Never, R = HNil>`: a lazy, `Send` effect over a typed capability list `R`, run
//! on a fiber runtime (design A, ADR 0011).
//!
//! - **[`Effect`]** — the core value: [`succeed`], [`fail`], [`sync`], [`from_async`],
//!   `map` / `flat_map` / `map_err` / `catch`, run at the edge with `Effect::block_on` or awaited
//!   with [`Runtime::run`]. [`LocalEffect`] holds `!Send` values.
//! - **[`effect!`](macro@effect)** — do-notation over an async body: `~Cap` reads a capability,
//!   `~expr` binds a sub-effect, `~(a, b)` joins in parallel; [`caps!`](macro@caps) spells `R`.
//! - **Typed `R`** — an HList of capabilities ([`HCons`], [`HNil`], [`need`], `provide`,
//!   `provide_env`), built from [`Layer`]s (`merge`, `provide_to`, scoped layers).
//! - **Fibers** — `fork` / `join` / interrupt, [`FiberRef`], [`Scope`], [`all`] / `zip_par` /
//!   [`race`], the [`Fabric`] admission scheduler, deterministic [`TestExecutor`].
//! - **Libraries** — [`Stream`] / [`Sink`], [`Schedule`] (`Effect::retry` / `repeat`),
//!   coordination ([`Queue`], [`Channel`], [`Deferred`], [`Semaphore`], [`PubSub`], [`Ref`], …),
//!   resources ([`Pool`], [`Cache`]), metrics and spans ([`MetricRegistry`], `with_span`),
//!   request batching and caching.
//! - **[`mod@stm`]** — software transactional memory ([`stm::TRef`], [`stm::TMap`],
//!   [`stm::commit`], …); a retrying transaction parks its fiber until a cell it read changes.
//! - **Kernel-independent** — [`failure`] (`Cause`, `Exit`, `Defect`, `Or`), [`foundation`]
//!   ([`Pipe`], `pipe!`), [`schema`], [`scheduling`] (`Duration`, `UtcDateTime`), [`match_`].
//!
//! ## Cargo features
//!
//! - **`compute`** (default) — the Compute Fabric ([`mod@compute`]): admission control, adaptive
//!   Rayon parallelism for bulk operations and `sysinfo` host telemetry. Without it, bulk
//!   operations run serially; `rayon` and `sysinfo` are not built.
//! - **`schema-serde`** — `serde_json::Value` ↔ [`Unknown`] and JSON Schema helpers.
//! - **`schema-regex`** — `schema::regex`: a `regex`-crate validator that exports its pattern.
//! - **`proptest`** — `testing::property`: proptest strategies for framework types and schemas,
//!   effect-aware properties under [`TestExecutor`], stateful (model-based) testing.
//!
//! Algebraic type classes and persistent (`im`) collections live in the maintenance-tier
//! `id_effect_fp` crate.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
// CI runs `cargo doc` with `RUSTDOCFLAGS=-D warnings`; allow rustdoc lints until intra-doc links are
// normalized (ambiguous modules, redundant explicit targets, etc.).
#![allow(rustdoc::all)]
// `effect-dylint-rules` targets crates that *compose* `Effect` at the edge; this crate implements
// the runtime (`Effect::block_on`, `Runtime`, …). Lints are crate-allowed here; see
// `.cursor/skills/effect.rs-fundamentals/SKILL.md` for caller-facing rules.
#![allow(unknown_lints)]
#![allow(
  effect_success_should_be_fn_type_param,
  effect_no_effect_suffix_on_graph_builder,
  effect_run_blocking_outside_boundary,
  effect_run_async_outside_boundary,
  effect_prefer_from_async_over_new_async,
  effect_from_async_single_await,
  effect_effect_generics_need_bounds,
  effect_multiple_top_level_effect_macros,
  effect_returning_effect_should_use_effect_macro,
  effect_no_async_fn_application,
  clippy::doc_overindented_list_items,
  clippy::should_implement_trait,
  clippy::type_complexity,
  clippy::unnecessary_lazy_evaluations
)]
// `#[cfg(test)]` law tests and similar intentionally exercise patterns Clippy flags (`unit` values,
// `clone` on `Copy` `Result`, etc.); keep `cargo clippy -p id_effect --all-targets -- -D warnings` green.
#![cfg_attr(
  test,
  allow(
    clippy::bool_assert_comparison,
    clippy::clone_on_copy,
    clippy::empty_line_after_doc_comments,
    clippy::let_unit_value,
    clippy::map_identity,
    clippy::redundant_closure,
    clippy::unit_arg,
    clippy::unit_cmp,
  )
)]

// Lets `::id_effect::…` in macro expansions resolve when those macros are used inside this crate.
extern crate self as id_effect;

/// Compiles and runs the README quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Compiles and runs the book's idiom guide snippets as doctests.
#[cfg(doctest)]
#[doc = include_str!("../book/src/appendix-e-idiom-guide.md")]
struct BookIdiomGuideDoctests;

/// Compiles and runs the book's interleaving-exploration snippets as doctests.
#[cfg(doctest)]
#[doc = include_str!("../book/src/part4/ch15-05-interleavings.md")]
struct BookInterleavingsDoctests;

pub use id_effect_macro::{err, pipe};
pub use id_effect_proc_macro::{
  EffectData, Optics, ProviderSpec as ProviderSpecDerive, Schema, caps, effect, effect_local,
  effect_tagged, match_effect,
};

// ── the 0.7 kernel ──────────────────────────────────────────────────────────────────────────
// Every module's items are glob re-exported at the root, so item names must be unique across
// them (`ambiguous_glob_reexports` is denied by clippy `-D warnings`).
pub mod capability;
pub mod compute;
pub mod concurrency;
pub mod coordination;
pub mod kernel;
pub mod layer;
pub mod observability;
pub mod par;
pub mod race;
pub mod random;
pub mod request;
pub mod resource;
pub mod runtime;
pub mod scheduling;
pub mod scope;
pub mod streaming;
pub mod testing;
// Not glob re-exported: `commit`, `Outcome`, `Txn` are too generic for the root namespace.
pub mod stm;

pub use capability::*;
pub use compute::*;
pub use concurrency::*;
pub use coordination::*;
pub use kernel::*;
pub use layer::*;
pub use observability::*;
pub use par::*;
pub use race::*;
pub use random::*;
pub use request::*;
pub use resource::*;
pub use runtime::*;
pub use scheduling::*;
pub use scope::*;
pub use streaming::*;
pub use testing::*;

#[doc(hidden)]
pub use kernel::macro_support as __private;

// ── kernel-independent modules ──────────────────────────────────────────────────────────────
pub mod failure;
pub mod foundation;
pub mod macros;
pub mod match_;
#[cfg(feature = "compute")]
pub mod parallelism;
pub mod schema;

pub use crate::match_::{HasTag, Matcher};
pub use failure::{Cause, Defect, Exit, FiberId, Never, Or};
#[doc(inline)]
pub use foundation::either::Either;
pub use foundation::func::{always, compose, identity};
pub use foundation::piping::Pipe;
pub use foundation::predicate::Predicate;
pub use schema::brand::Brand;
pub use schema::data::{DataError, DataStruct, DataTuple, EffectData as EffectDataTrait};
pub use schema::equal::{EffectHash, Equal};
pub use schema::order::{DynOrder, Ordering, ordering};
pub use schema::{HasSchema, ParseError, ParseErrors, Redacted, Schema, Unknown};
