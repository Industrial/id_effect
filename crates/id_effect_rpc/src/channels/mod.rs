//! Realtime channels: topics with join and leave, presence, and broadcast across the mesh
//! (feature `channels`; web.realtime #3–#6, ADR 0014 D3: a module of `id_effect_rpc`, not a
//! crate).
//!
//! A **topic** is a named room. A connection **joins** it through a [`JoinPolicy`] (the default
//! denies every join), receives a snapshot of who is there, then every message published on the
//! topic — on any node — and every join and leave ([`ChannelEvent`]). Each node runs one **hub
//! process** on the process mesh; a topic is a mesh process group (`id_effect_node`'s
//! `ProcessGroups` CRDT) whose members are the hubs of the nodes with subscribers on it, so a
//! publish is one cast per interested node, not one per subscriber.
//!
//! ```rust
//! use std::sync::Arc;
//! use id_effect_node::{BehaviorRegistry, Mesh, MeshFabric, NodeId};
//! use id_effect_process::{NodeName, ProcessNode};
//! use id_effect_rpc::channels::{AuthzJoin, ChannelEvent, Channels, register_hub};
//! use id_effect_authz::require_role;
//!
//! // Every node registers the hub behavior before it builds its mesh.
//! let mut registry = BehaviorRegistry::new();
//! register_hub(&mut registry);
//! let name = NodeName::new("a@docs");
//! let mesh = Mesh::new(NodeId::fresh(&name), ProcessNode::new(name), registry)
//!   .with_fabric(MeshFabric::new());
//!
//! // Messages are any serde type; members of the role "member" may join.
//! let chat = Channels::<String>::builder()
//!   .join_policy(AuthzJoin::new(require_role("member")))
//!   .start_on_mesh(&mesh)
//!   .expect("hub started");
//! assert_eq!(chat.node(), "a@docs");
//! # let _ = Arc::clone(&mesh);
//! ```
//!
//! ## Delivery
//!
//! - **Order per publisher.** A node numbers what it publishes; a receiving hub delivers a
//!   node's messages in that order and drops a duplicate or a late arrival (a retransmission
//!   overtaken by a newer message), so a subscriber never sees a node's messages out of order.
//! - **Live, not durable.** A subscriber receives what is published while it is joined. History
//!   and catch-up are a subscription on the event log (`subscription::events`), with the topic as
//!   the projection. The mesh delivers casts at most once by default; `Mesh::with_delivery`
//!   (at-least-once) makes the hub-to-hub casts reliable across reconnects.
//! - **Bounded.** Each subscriber has a buffer of [`ChannelsConfig::subscriber_buffer`] events; a
//!   subscriber that falls that far behind is dropped with `{"_tag": "Lagged"}` (409) and
//!   rejoins (with history from the log if it needs it). Nothing is queued without a bound.
//!
//! ## Presence
//!
//! Every joined connection is a [`Presence`] (principal key, connection, node, metadata). Joins
//! and leaves are pushed to the topic's subscribers as [`PresenceDiff`]s. Hubs send their members
//! to each other on join and leave and, as a heartbeat, every
//! [`ChannelsConfig::heartbeat_interval`]; a remote member not refreshed for
//! [`ChannelsConfig::presence_ttl`] lapses, and a node that goes down (its hub's monitor fires
//! with `noconnection`) loses its members at once. Caps: members per topic, topics per
//! connection, topics per node ([`ChannelsConfig`]).
//!
//! ## Security
//!
//! - Join authorization runs on **every** join, subscribe and resume ([`JoinPolicy`];
//!   [`DenyAllJoins`] by default; [`dangerous_allow_all_joins`] is a recorded opt-out). The
//!   procedure serving joins still needs its own access decision (`.public()` / `.guard(g)`).
//! - The joining principal is the `id_effect_authz` [`CurrentPrincipal`] in the call's
//!   environment (anonymous without one); the connection is the [`ConnectionId`] the transport
//!   puts there.
//! - Errors carry their tag only; policy reasons and payload details stay in the server log.
//!
//! ## The cross-node seam
//!
//! [`Backplane`] is everything the hub needs from the network: interest per topic, fan-out to
//! the interested nodes, and a heartbeat. [`MeshBackplane`] implements it on the mesh's process
//! groups and casts; a later transport (mesh RPC) replaces it without changing [`Channels`].
//!
//! [`CurrentPrincipal`]: id_effect_authz::CurrentPrincipal

use std::time::Duration;

use http::StatusCode;
use serde_json::{Value, json};

use crate::error_union::{RpcErrorStatus, RpcErrorUnion};

mod backplane;
mod clock;
mod hub;
mod policy;
mod presence;
mod rpc;

pub use backplane::{
  Backplane, HUB_BEHAVIOR, Inbound, MeshBackplane, SessionLink, attach_session, attach_session_rpc,
  connect_in_memory, register_hub,
};
pub use clock::{ChannelClock, ManualClock, SystemClock};
pub use hub::{
  ChannelEvent, ChannelMessage, Channels, ChannelsBuilder, HeartbeatTask, JoinSpec, Membership,
  Published,
};
pub use policy::{
  AllowAllJoins, AuthzJoin, DANGEROUS_ALLOW_ALL_JOINS, DenyAllJoins, FnJoinPolicy, JoinPolicy,
  JoinRequest, JoinTarget, dangerous_allow_all_joins, join_policy_fn,
};
pub use presence::{Presence, PresenceDiff};
pub use rpc::{ConnectionId, JoinInput};

/// The book chapter on channels runs as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../../../id_effect/book/src/part6/ch33-09-channels-presence.md")]
struct BookChannelsDoctests;

/// The longest topic name, in bytes.
pub const MAX_TOPIC_LEN: usize = 128;

/// The largest message payload [`ChannelsConfig::with_max_message_bytes`] accepts (postcard
/// bytes), below the 64 KiB mesh frame (design note §5, C3) with room for the envelope.
pub const MAX_MESSAGE_BYTES: usize = 60 * 1024;

/// Limits and timings of one node's channels. Every value is clamped to a fixed range, so no
/// configuration makes a node hold an unbounded number of topics, members or events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelsConfig {
  max_members_per_topic: usize,
  max_topics_per_connection: usize,
  max_topics: usize,
  max_meta_bytes: usize,
  max_message_bytes: usize,
  subscriber_buffer: usize,
  heartbeat_interval: Duration,
  presence_ttl: Duration,
}

impl Default for ChannelsConfig {
  fn default() -> Self {
    Self {
      max_members_per_topic: 10_000,
      max_topics_per_connection: 32,
      max_topics: 10_000,
      max_meta_bytes: 1024,
      max_message_bytes: 16 * 1024,
      subscriber_buffer: 256,
      heartbeat_interval: Duration::from_secs(5),
      presence_ttl: Duration::from_secs(15),
    }
  }
}

impl ChannelsConfig {
  /// The defaults: 10,000 members per topic, 32 topics per connection, 10,000 topics per node,
  /// 1 KiB of metadata, 16 KiB messages, 256 buffered events per subscriber, a heartbeat every
  /// 5 s and remote members lapsing after 15 s without one.
  pub fn new() -> Self {
    Self::default()
  }

  /// Members per topic, as this node sees the topic (clamped to `1..=100_000`).
  #[must_use]
  pub fn with_max_members_per_topic(mut self, n: usize) -> Self {
    self.max_members_per_topic = n.clamp(1, 100_000);
    self
  }

  /// Topics one connection may join (clamped to `1..=1_024`).
  #[must_use]
  pub fn with_max_topics_per_connection(mut self, n: usize) -> Self {
    self.max_topics_per_connection = n.clamp(1, 1_024);
    self
  }

  /// Topics with subscribers on this node (clamped to `1..=1_000_000`).
  #[must_use]
  pub fn with_max_topics(mut self, n: usize) -> Self {
    self.max_topics = n.clamp(1, 1_000_000);
    self
  }

  /// Bytes of JSON metadata a connection may join with (clamped to `0..=16 KiB`).
  #[must_use]
  pub fn with_max_meta_bytes(mut self, n: usize) -> Self {
    self.max_meta_bytes = n.min(16 * 1024);
    self
  }

  /// Bytes of one published message (clamped to `1..=`[`MAX_MESSAGE_BYTES`]).
  #[must_use]
  pub fn with_max_message_bytes(mut self, n: usize) -> Self {
    self.max_message_bytes = n.clamp(1, MAX_MESSAGE_BYTES);
    self
  }

  /// Events buffered per subscriber before it is dropped as lagged (clamped to `2..=65_536`;
  /// the join snapshot takes one).
  #[must_use]
  pub fn with_subscriber_buffer(mut self, n: usize) -> Self {
    self.subscriber_buffer = n.clamp(2, 65_536);
    self
  }

  /// The heartbeat period (clamped to `100 ms..=60 s`); the presence TTL is raised to at least
  /// two periods.
  #[must_use]
  pub fn with_heartbeat_interval(mut self, every: Duration) -> Self {
    self.heartbeat_interval = every.clamp(Duration::from_millis(100), Duration::from_secs(60));
    self.presence_ttl = self.presence_ttl.max(self.heartbeat_interval * 2);
    self
  }

  /// How long a remote member lives without a heartbeat (at least two heartbeat periods, at
  /// most 10 minutes).
  #[must_use]
  pub fn with_presence_ttl(mut self, ttl: Duration) -> Self {
    self.presence_ttl = ttl.clamp(self.heartbeat_interval * 2, Duration::from_secs(600));
    self
  }

  /// Members per topic.
  pub fn max_members_per_topic(&self) -> usize {
    self.max_members_per_topic
  }

  /// Topics per connection.
  pub fn max_topics_per_connection(&self) -> usize {
    self.max_topics_per_connection
  }

  /// Topics per node.
  pub fn max_topics(&self) -> usize {
    self.max_topics
  }

  /// Metadata bytes per member.
  pub fn max_meta_bytes(&self) -> usize {
    self.max_meta_bytes
  }

  /// Bytes per message.
  pub fn max_message_bytes(&self) -> usize {
    self.max_message_bytes
  }

  /// Events buffered per subscriber.
  pub fn subscriber_buffer(&self) -> usize {
    self.subscriber_buffer
  }

  /// The heartbeat period.
  pub fn heartbeat_interval(&self) -> Duration {
    self.heartbeat_interval
  }

  /// The remote member lifetime without a heartbeat.
  pub fn presence_ttl(&self) -> Duration {
    self.presence_ttl
  }
}

/// Why a join, a publish or a subscription failed. On the wire only the tag crosses
/// (`{"_tag": "JoinDenied"}`); the reason stays in the server log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
  /// The join policy denied an authenticated principal (403).
  #[error("the join policy denied the join")]
  JoinDenied,
  /// The join policy denied an anonymous principal: authenticate first (401).
  #[error("joining this topic requires an authenticated principal")]
  Unauthenticated,
  /// The topic name is empty, longer than [`MAX_TOPIC_LEN`] bytes, or holds control characters
  /// (400).
  #[error("invalid topic name")]
  InvalidTopic,
  /// The join metadata is larger than [`ChannelsConfig::max_meta_bytes`] (413).
  #[error("join metadata is too large")]
  MetaTooLarge,
  /// The message is larger than [`ChannelsConfig::max_message_bytes`] (413).
  #[error("message is too large")]
  MessageTooLarge,
  /// The topic has [`ChannelsConfig::max_members_per_topic`] members (409).
  #[error("the topic is full")]
  TopicFull,
  /// The connection already joined this topic (409).
  #[error("the connection already joined this topic")]
  AlreadyJoined,
  /// The connection has joined [`ChannelsConfig::max_topics_per_connection`] topics, or the node
  /// holds [`ChannelsConfig::max_topics`] (429).
  #[error("too many topics")]
  TooManyTopics,
  /// The subscriber fell [`ChannelsConfig::subscriber_buffer`] events behind and was dropped
  /// (409); rejoin, with history from the log if needed.
  #[error("the subscriber fell behind and was dropped")]
  Lagged,
  /// The channels of this node were shut down (503).
  #[error("channels are shut down")]
  Closed,
  /// The join policy failed, or a message could not be encoded (500, answered as
  /// `InternalError { id }`).
  #[error("channels internal error: {0}")]
  Internal(String),
}

impl ChannelError {
  const TAGS: [&'static str; 10] = [
    "JoinDenied",
    "Unauthenticated",
    "InvalidTopic",
    "MetaTooLarge",
    "MessageTooLarge",
    "TopicFull",
    "AlreadyJoined",
    "TooManyTopics",
    "Lagged",
    "Closed",
  ];

  /// The wire tag (`"Internal"` for internal errors, which never reach the wire as such).
  pub fn tag(&self) -> &'static str {
    match self {
      Self::JoinDenied => "JoinDenied",
      Self::Unauthenticated => "Unauthenticated",
      Self::InvalidTopic => "InvalidTopic",
      Self::MetaTooLarge => "MetaTooLarge",
      Self::MessageTooLarge => "MessageTooLarge",
      Self::TopicFull => "TopicFull",
      Self::AlreadyJoined => "AlreadyJoined",
      Self::TooManyTopics => "TooManyTopics",
      Self::Lagged => "Lagged",
      Self::Closed => "Closed",
      Self::Internal(_) => "Internal",
    }
  }
}

impl RpcErrorStatus for ChannelError {
  fn status(&self) -> StatusCode {
    match self {
      Self::JoinDenied => StatusCode::FORBIDDEN,
      Self::Unauthenticated => StatusCode::UNAUTHORIZED,
      Self::InvalidTopic => StatusCode::BAD_REQUEST,
      Self::MetaTooLarge | Self::MessageTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
      Self::TopicFull | Self::AlreadyJoined | Self::Lagged => StatusCode::CONFLICT,
      Self::TooManyTopics => StatusCode::TOO_MANY_REQUESTS,
      Self::Closed => StatusCode::SERVICE_UNAVAILABLE,
      Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
  }
}

// Kept out of rustc's "other types implement `RpcErrorUnion`" list, so the UI snapshot
// `tests/ui/error_type_not_a_union.stderr` is the same with and without `channels` (F31).
#[diagnostic::do_not_recommend]
impl RpcErrorUnion for ChannelError {
  fn to_wire(&self) -> Value {
    json!({"_tag": self.tag()})
  }

  fn is_internal(&self) -> bool {
    matches!(self, Self::Internal(_))
  }

  fn union_schema() -> Option<Value> {
    let variants: Vec<Value> = Self::TAGS
      .iter()
      .map(|tag| {
        json!({
          "type": "object",
          "properties": {"_tag": {"const": tag}},
          "required": ["_tag"]
        })
      })
      .collect();
    Some(json!({"oneOf": variants}))
  }
}

/// `topic` is 1..=[`MAX_TOPIC_LEN`] bytes without control characters.
pub(crate) fn check_topic(topic: &str) -> Result<(), ChannelError> {
  if topic.is_empty() || topic.len() > MAX_TOPIC_LEN || topic.chars().any(char::is_control) {
    return Err(ChannelError::InvalidTopic);
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config_values_are_clamped() {
    let c = ChannelsConfig::new()
      .with_max_members_per_topic(0)
      .with_max_topics_per_connection(usize::MAX)
      .with_max_topics(0)
      .with_max_meta_bytes(usize::MAX)
      .with_max_message_bytes(usize::MAX)
      .with_subscriber_buffer(0)
      .with_heartbeat_interval(Duration::from_secs(3600))
      .with_presence_ttl(Duration::ZERO);
    assert_eq!(c.max_members_per_topic(), 1);
    assert_eq!(c.max_topics_per_connection(), 1_024);
    assert_eq!(c.max_topics(), 1);
    assert_eq!(c.max_meta_bytes(), 16 * 1024);
    assert_eq!(c.max_message_bytes(), MAX_MESSAGE_BYTES);
    assert_eq!(c.subscriber_buffer(), 2);
    assert_eq!(c.heartbeat_interval(), Duration::from_secs(60));
    assert_eq!(
      c.presence_ttl(),
      Duration::from_secs(120),
      "two heartbeats at least"
    );
    let d = ChannelsConfig::default();
    assert_eq!(d.presence_ttl(), Duration::from_secs(15));
    assert_eq!(d.subscriber_buffer(), 256);
  }

  #[test]
  fn errors_are_tagged_with_statuses() {
    let all = [
      (ChannelError::JoinDenied, 403),
      (ChannelError::Unauthenticated, 401),
      (ChannelError::InvalidTopic, 400),
      (ChannelError::MetaTooLarge, 413),
      (ChannelError::MessageTooLarge, 413),
      (ChannelError::TopicFull, 409),
      (ChannelError::AlreadyJoined, 409),
      (ChannelError::TooManyTopics, 429),
      (ChannelError::Lagged, 409),
      (ChannelError::Closed, 503),
    ];
    for (err, status) in &all {
      assert_eq!(err.status().as_u16(), *status);
      assert_eq!(err.to_wire(), json!({"_tag": err.tag()}));
      assert!(!err.is_internal());
      assert!(!err.to_string().is_empty());
    }
    let internal = ChannelError::Internal("x".into());
    assert!(internal.is_internal());
    assert_eq!(internal.status().as_u16(), 500);
    assert_eq!(internal.tag(), "Internal");
    let schema = ChannelError::union_schema().unwrap();
    assert_eq!(schema["oneOf"].as_array().unwrap().len(), all.len());
  }

  #[test]
  fn topic_names_are_checked() {
    assert!(check_topic("room:general").is_ok());
    assert_eq!(check_topic(""), Err(ChannelError::InvalidTopic));
    assert_eq!(check_topic("a\nb"), Err(ChannelError::InvalidTopic));
    assert_eq!(
      check_topic(&"x".repeat(MAX_TOPIC_LEN + 1)),
      Err(ChannelError::InvalidTopic)
    );
  }
}
