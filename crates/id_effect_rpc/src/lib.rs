//! Typed RPC for [`id_effect`](https://docs.rs/id_effect): the first-party call surface of
//! ADR 0014 ("One call surface").
//!
//! A service is a set of tagged procedures. Each procedure has a kind (query, mutation, server
//! stream, client stream, two-way stream, subscription), input and output schemas, typed errors,
//! and an access decision — public or guarded — without which it does not register (C1). The
//! [`contract`] describes the services for generated clients and compatibility checks; the
//! [`wire`] types and [`codec`]s fix what crosses the network.
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect::{DynEnv, schema, succeed};
//! use id_effect_rpc::{Procedure, RpcGroup, RpcServer};
//!
//! let mut group = RpcGroup::new();
//! group.register(
//!   Procedure::query("Greeter.greet").public(),
//!   Arc::new(schema::string::<()>()),
//!   Arc::new(schema::string::<()>()),
//!   &["message"],
//!   |name, _env| succeed(format!("Hello, {name}!")),
//! );
//! let contract = group.service_contract("Greeter", 1);
//! assert_eq!(contract.procedures[0].name, "greet");
//! let app: axum::Router = RpcServer::new(group).mount_default().with_state(DynEnv::new());
//! # let _ = app;
//! ```
//!
//! ## Modules
//!
//! - [`contract`] — the serde contract model and its golden snapshot (D11)
//! - [`wire`] — call envelope, exit union, reserved failures, frames, cursors, sessions
//! - [`codec`] — the [`Codec`](codec::Codec) trait, JSON, negotiation (D13)
//! - [`registry`] — [`Procedure`] (access typestate, kind, version) and [`RpcGroup`]
//! - [`handler`] — the handler shapes of the six kinds and their erased, transport-independent
//!   form; [`kinds`] — [`Duplex`], [`Subscription`], the frames of a running call, the
//!   in-process transport (feature `test-util`)
//! - [`error_union`] — typed error unions `{"_tag": …}`; [`cancel`] — deadlines and cancellation;
//!   [`idempotency`] — `Idempotency-Key` replay for mutations
//! - [`server`] — [`RpcServer`]: the `/rpc` routes as declared typed routes, [`server::dispatch`]
//! - [`client`] — [`RpcClient`] (feature `client`, default)
//! - [`protocol`], [`serialization`] — the dispatch path constant, tuple-Schema bridging
//! - [`correlation`], [`span`], [`versioning`], [`openapi`], [`RpcError`] — edge helpers
//!
//! `#[rpc::service]` ([`service`](macro@service), re-exported from `id_effect_rpc_macros`)
//! declares a service on a trait. The wire and contract formats are specified in the 0.12
//! design note (`.maestro/specs/releases/0.12/r12-rpc-design.md`).
//!
//! Later 0.12 modules are added below, one `pub mod` / `pub use` line each.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::missing_panics_doc)]

/// The README's examples run as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

pub mod batch;
pub mod cancel;
#[cfg(feature = "client")]
pub mod client;
pub mod codec;
pub mod contract;
pub mod correlation;
mod envelope;
pub mod error;
pub mod error_union;
pub mod handler;
pub mod idempotency;
pub mod kinds;
pub mod openapi;
pub mod protocol;
pub mod registry;
pub mod serialization;
pub mod server;
pub mod span;
pub mod versioning;
pub mod wire;

#[cfg(feature = "client")]
pub use client::{RpcClient, RpcClientConfig, RpcClientError};
pub use contract::{Contract, ProcedureAccess, ProcedureContract, ProcedureKind, ServiceContract};
pub use envelope::{RpcEnvelope, RpcErrorCode};
pub use error::RpcError;
pub use error_union::{RpcErrorStatus, RpcErrorUnion};
pub use handler::WireSchema;
pub use id_effect_rpc_macros::service;
pub use kinds::{Duplex, Subscription, SubscriptionItem};
pub use protocol::RPC_DISPATCH_PATH;
pub use registry::{Procedure, RpcGroup, RpcMethodEntry};
pub use server::{RpcPrincipal, RpcServer, layer_rpc};

// r12-e12-2-subscriptions: subscription sources (feature `events`: the event log).
pub mod subscription;

// `#[rpc::service]` support (r12-e12-1-macro); `service` the module and `service` the attribute
// live in different namespaces.
pub mod service;
pub use id_effect_rpc_macros::RpcErrorUnion;

// r12-e12-4-openapi-asyncapi: AsyncAPI 3.0 of the stream kinds and topics (`openapi` above
// holds OpenAPI 3.1 and the served contract documents).
pub mod asyncapi;
// r12-e12-2-channels: topics, presence and mesh broadcast (feature `channels`).
#[cfg(feature = "channels")]
pub mod channels;
// r12-e12-2-mesh-rpc: the transports besides HTTP (`transport::mesh`, feature `mesh`).
pub mod transport;
// r12-e12-2-bidi-resume: resume sessions, replay buffers, cursor and resume decoders.
pub mod resume;
