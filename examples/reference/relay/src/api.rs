//! The Relay's RPC contract (ADR 0014): [`Relay`], the service clients call (HTTP/1.1, HTTP/2,
//! HTTP/3, WebSocket, WebTransport), and [`RelayPeer`], the service nodes call on each other over
//! the mesh. Every procedure declares who may call it: a signed-in user ([`crate::auth::user`])
//! or another verified mesh node ([`MeshPeerGuard`]); nothing is public.
//!
//! The contract is checked in as `rpc.contract.json` ([`crate::contract`]); the TypeScript client
//! under `web/src/rpc` is generated from it (`id-effect rpc gen ts`).

use id_effect::{DynEnv, Effect};
use id_effect_kit::rpc;
use id_effect_kit::rpc::kinds::{Duplex, Subscription};
use id_effect_kit::rpc::transport::mesh::MeshPeerGuard;
use id_effect_kit::rpc::wire::Cursor;
use serde::{Deserialize, Serialize};

use crate::auth::user;

/// The longest room name (lower-case letters, digits and `-`).
pub const MAX_ROOM_LEN: usize = 64;

/// The longest message text, in characters.
pub const MAX_TEXT_LEN: usize = 2_000;

/// The longest presence status, in characters.
pub const MAX_STATUS_LEN: usize = 32;

/// The longest user id ([`Invite::user`]), in characters.
pub const MAX_USER_LEN: usize = 256;

/// Asks for a user to be added to a room ([`Relay::invite`]).
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct Invite {
  /// The room.
  pub room: String,
  /// The user to add (the `sub` of their token).
  pub user: String,
}

/// A message to post ([`Relay::chat`], [`Relay::send`]).
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct Outgoing {
  /// The room.
  pub room: String,
  /// The text (1 to [`MAX_TEXT_LEN`] characters).
  pub text: String,
}

/// A posted message, as stored: its room and its number in the room.
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct Ack {
  /// The room.
  pub room: String,
  /// The message's number in the room (1, 2, …).
  pub seq: i64,
}

/// A message of a room ([`Relay::history`], and the live `message` events of [`Relay::join`]).
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct ChatMessage {
  /// The room.
  pub room: String,
  /// Its number in the room (1, 2, …, no gaps).
  pub seq: i64,
  /// Who posted it.
  pub from: String,
  /// The node it was posted on.
  pub node: String,
  /// The text.
  pub text: String,
}

/// Enter a room ([`Relay::join`]) with a presence status (`online`, `away`, …).
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct JoinRoom {
  /// The room.
  pub room: String,
  /// The status the others see (at most [`MAX_STATUS_LEN`] characters).
  pub status: String,
}

/// One connection present in a room.
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct Member {
  /// The user.
  pub user: String,
  /// The connection (one user may be present on several).
  pub conn: String,
  /// The node the connection is on.
  pub node: String,
  /// The status it joined with.
  pub status: String,
}

/// One event of a joined room: `joined` (first: who is there), `message` (posted on any node)
/// or `presence` (who came and went).
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct RoomEvent {
  /// `joined`, `message` or `presence`.
  pub kind: String,
  /// The room.
  pub room: String,
  /// `joined`: everyone present, this connection included.
  pub members: Vec<Member>,
  /// `presence`: who joined.
  pub joins: Vec<Member>,
  /// `presence`: who left.
  pub leaves: Vec<Member>,
  /// `message`: the message.
  pub message: Option<ChatMessage>,
}

/// How many connections a room has on one node.
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct RoomCount {
  /// The room.
  pub room: String,
  /// Connections present on the node.
  pub present: i64,
}

/// One node as [`Relay::cluster`] reports it.
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct NodeStatus {
  /// The node (`name@host`).
  pub node: String,
  /// Whether it answered.
  pub up: bool,
  /// Its rooms with connections, by name.
  pub rooms: Vec<RoomCount>,
}

/// Every node the asked node knows ([`Relay::cluster`]): itself first, then its peers by name.
#[derive(Clone, Debug, PartialEq, Eq, id_effect::Schema, Serialize, Deserialize)]
pub struct ClusterView {
  /// The nodes.
  pub nodes: Vec<NodeStatus>,
}

/// The typed failures of the Relay's procedures: part of the contract, decoded by every client.
#[derive(Clone, Debug, PartialEq, Eq, rpc::RpcErrorUnion)]
pub enum RelayError {
  /// A room name is empty, too long or not `[a-z0-9-]`.
  #[rpc(status = 422)]
  InvalidRoom {
    /// The longest name accepted.
    max: i64,
  },
  /// A message text or a status is empty or too long.
  #[rpc(status = 422)]
  InvalidText {
    /// The longest text accepted.
    max: i64,
  },
  /// The caller is not a member of the room.
  #[rpc(status = 403)]
  NotAMember {
    /// The room.
    room: String,
  },
  /// A room of that name exists.
  #[rpc(status = 409)]
  RoomExists {
    /// The room.
    room: String,
  },
  /// The connection fell too far behind the room's live events; join again (and catch up with
  /// [`Relay::history`]).
  #[rpc(status = 409)]
  Lagged,
  /// A history cursor that is not an event-log cursor.
  #[rpc(status = 400)]
  InvalidCursor,
  /// The node is at a limit (connections per room, rooms per connection) or shutting down.
  #[rpc(status = 503)]
  Busy,
  /// A fault on the server (database, mesh); logged with its detail, answered without it.
  #[rpc(internal)]
  Internal {
    /// What failed (server log only).
    detail: String,
  },
}

/// A chat across nodes: rooms with members, live presence, messages over a two-way stream and
/// a resumable history. Version 1 of the `Relay` contract.
#[rpc::service(version = 1)]
pub trait Relay {
  /// Creates the room `room` with the caller as its first member.
  #[mutation]
  #[guard(user())]
  fn create_room(&self, room: String) -> Effect<(), RelayError, DynEnv>;

  /// Adds a user to a room the caller is a member of.
  #[mutation]
  #[guard(user())]
  fn invite(&self, invite: Invite) -> Effect<(), RelayError, DynEnv>;

  /// Posts messages: one acknowledgement per message, in order, once it is stored and
  /// published to every node.
  #[bidi]
  #[guard(user())]
  fn chat(&self) -> Effect<Duplex<Outgoing, Ack, RelayError>, RelayError, DynEnv>;

  /// Posts one message (for clients without a stream transport).
  #[mutation]
  #[guard(user())]
  fn send(&self, message: Outgoing) -> Effect<Ack, RelayError, DynEnv>;

  /// Enters a room the caller is a member of: who is there, then every message and every
  /// arrival and departure, on any node, until the call ends. Members only.
  #[subscription]
  #[guard(user())]
  fn join(&self, join: JoinRoom)
  -> Effect<Subscription<RoomEvent, RelayError>, RelayError, DynEnv>;

  /// Every message of a room from the start (or after `after`), then each new one: resume with
  /// the cursor of the last message seen and nothing is missed or repeated. Members only.
  #[subscription]
  #[guard(user())]
  fn history(
    &self,
    room: String,
    after: Option<Cursor>,
  ) -> Effect<Subscription<ChatMessage, RelayError>, RelayError, DynEnv>;

  /// The nodes this node knows and the rooms present on each, asked over the mesh.
  #[query]
  #[guard(user())]
  fn cluster(&self) -> Effect<ClusterView, RelayError, DynEnv>;
}

/// What one node asks another over the mesh (mesh RPC, postcard): callable by verified nodes
/// only, never by end users. Version 1 of the `RelayPeer` contract.
#[rpc::service(version = 1)]
pub trait RelayPeer {
  /// This node's rooms with connections.
  #[query]
  #[guard(MeshPeerGuard::any_node())]
  fn status(&self) -> Effect<NodeStatus, RelayError, DynEnv>;
}

/// Whether `room` is a valid room name: 1 to [`MAX_ROOM_LEN`] of `[a-z0-9-]`.
pub fn valid_room(room: &str) -> bool {
  !room.is_empty()
    && room.len() <= MAX_ROOM_LEN
    && room
      .bytes()
      .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The channels topic (and event-log stream) of `room`.
pub fn topic(room: &str) -> String {
  format!("room:{room}")
}

/// The room of a topic made by [`topic`].
pub fn room_of(topic: &str) -> Option<&str> {
  topic.strip_prefix("room:")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn room_names_are_checked() {
    assert!(valid_room("general"));
    assert!(valid_room("team-7"));
    assert!(!valid_room(""));
    assert!(!valid_room("General"));
    assert!(!valid_room("a b"));
    assert!(!valid_room(&"a".repeat(MAX_ROOM_LEN + 1)));
    assert_eq!(topic("general"), "room:general");
    assert_eq!(room_of("room:general"), Some("general"));
    assert_eq!(room_of("general"), None);
  }
}
