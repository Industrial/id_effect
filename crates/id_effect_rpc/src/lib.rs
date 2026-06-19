//! RPC-style HTTP helpers for [`id_effect`](https://docs.rs/id_effect) Axum hosts — **Phase D**
//! (`@effect/rpc`-shaped boundaries).
//!
//! ## Full stack (D3)
//!
//! - [`protocol`] — tagged wire request/response types
//! - [`serialization`] — JSON + [`id_effect::schema`] encode/decode
//! - [`registry`] — [`RpcGroup`] of typed handlers
//! - [`server`] — [`RpcServer`] Axum dispatch (`POST /rpc`)
//! - [`client`] — [`RpcClient`] remote calls via [`id_effect_platform::http::HttpClient`]
//! - [`stream`] — NDJSON stream RPC responses
//!
//! ## Edge helpers (D2)
//!
//! - [`RpcError`] — JSON envelope + status + [`IntoResponse`](axum::response::IntoResponse)
//! - [`correlation`] — `x-correlation-id` propagation
//! - [`codegen`] — service metadata and Rust trait stub emission
//! - [`openapi`] — OpenAPI 3.0 JSON/YAML emission from route metadata
//! - [`span`] — `tracing` span helpers compatible with OpenTelemetry layers
//! - [`versioning`] — API version negotiation middleware
//!
//! Pair with **`id_effect_axum::json`** (`decode_json_schema`, `JsonSchemaError`) for
//! per-route JSON validation (see the mdBook *Axum host* chapter).

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![allow(clippy::missing_panics_doc)]

pub mod client;
pub mod codegen;
pub mod correlation;
mod envelope;
pub mod error;
pub mod openapi;
pub mod protocol;
pub mod registry;
pub mod serialization;
pub mod server;
pub mod span;
pub mod stream;
pub mod versioning;

pub use client::{RpcClient, RpcClientConfig, RpcClientError};
pub use envelope::{RpcEnvelope, RpcErrorCode};
pub use error::RpcError;
pub use protocol::{RPC_DISPATCH_PATH, RpcWireRequest, RpcWireResponse};
pub use registry::{RpcGroup, RpcMethodEntry};
pub use server::{RpcServer, layer_rpc};
