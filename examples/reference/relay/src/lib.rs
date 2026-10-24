//! Relay — the id_effect 0.12 reference app: a chat across three (or more) nodes with
//! presence, built only through `id_effect_kit` on the one-call surface (ADR 0014).
//!
//! - **Rooms** are channels topics ([`id_effect_kit::rpc::channels`]): joining is a
//!   subscription ([`api::Relay::join`]) admitted by a membership policy
//!   ([`rooms::MembersOnly`]: members only, deny by default); presence is shared by the nodes'
//!   hubs over the mesh.
//! - **Messages** go over a two-way RPC stream ([`api::Relay::chat`]): each is appended to the
//!   room's stream in a PostgreSQL event log and published on the room's channel.
//! - **History** is a subscription on the event log ([`api::Relay::history`]): the cursor is the
//!   event's position, so a client that reconnects resumes with no gap and no repeat.
//! - **Nodes** talk over mesh RPC ([`api::RelayPeer`], guarded for verified nodes only) on a
//!   QUIC endpoint of the mesh's own, with mutual TLS on `idm/1`; each session runs on a node
//!   session pump that verifies every envelope.
//! - **The host** ([`node::RelayNode`] starts the host entry point) serves the clients on one
//!   port number: TLS on TCP, and HTTP/3 with WebTransport on UDP (`server.http3`,
//!   `server.http3.webtransport`), with no HTTP/3 endpoint of the Relay's own.
//! - **Clients**: the generated Rust client over HTTP (HTTP/1.1, HTTP/2, HTTP/3) and WebSocket,
//!   the generated TypeScript client (`web/src/rpc`), and the browser page (`web/`), which
//!   prefers WebTransport and falls back to WebSocket.
//!
//! The startup posture check passes under `prod` with no accepted risk ([`node::RelayNode::posture`]).
//!
//! ```rust
//! use relay::api::{RelayService, valid_room};
//!
//! assert!(valid_room("general"));
//! assert_eq!(RelayService::NAME, "Relay");
//! assert_eq!(relay::contract().services.len(), 2);
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod api;
pub mod auth;
pub mod config;
pub mod handlers;
pub mod idempotency;
pub mod node;
pub mod rooms;

use id_effect_kit::rpc::contract::Contract;

/// The README's Rust example runs as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Every RPC service the Relay serves, as one contract document (`rpc.contract.json`): the
/// client-facing `Relay` and the node-to-node `RelayPeer`.
pub fn contract() -> Contract {
  Contract::new()
    .with_service(api::RelayService::contract())
    .with_service(api::RelayPeerService::contract())
}
