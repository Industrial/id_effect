//! The handlers of [`Relay`] and [`RelayPeer`] on one node: rooms and members in PostgreSQL,
//! messages appended to the room's event stream and published on its channel (every node with
//! someone in the room gets one cast), history read back from the event log, and the cluster
//! view asked of every peer over mesh RPC.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use id_effect::{DynEnv, Effect, Exit, Stream, fail, succeed};
use id_effect_kit::authz::CurrentPrincipal;
use id_effect_kit::events::{EventStore, EventStoreError, RecordedEvent, StoredEvent};
use id_effect_kit::rpc::channels::{ChannelError, ChannelEvent, Channels, JoinSpec, Presence};
use id_effect_kit::rpc::client::mesh::MeshTransport;
use id_effect_kit::rpc::kinds::{Duplex, Subscription, SubscriptionItem};
use id_effect_kit::rpc::subscription::events::EventSubscription;
use id_effect_kit::rpc::transport::mesh::MeshRpc;
use id_effect_kit::rpc::wire::Cursor;
use id_effect_kit::sql::SqlError;
use serde_json::{Value, json};

use crate::api::{
  Ack, ChatMessage, ClusterView, Invite, JoinRoom, MAX_ROOM_LEN, MAX_STATUS_LEN, MAX_TEXT_LEN,
  MAX_USER_LEN, Member, NodeStatus, Outgoing, Relay, RelayError, RelayPeer, RelayPeerService,
  RoomCount, RoomEvent, room_of, topic, valid_room,
};
use crate::auth::{connection, principal};
use crate::rooms::{Journal, Posted, Rooms, on_dyn};

/// How long [`Relay::cluster`] waits for one peer.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(2);

/// The mesh service name of `node`'s [`RelayPeer`] endpoint (one per node: mesh endpoints have
/// cluster-wide names).
pub fn peer_service(node: &str) -> String {
  format!("RelayPeer@{node}")
}

/// What the handlers of one node share.
pub struct NodeState {
  node: String,
  rooms: Rooms,
  journal: Journal,
  history: EventSubscription<Posted>,
  channels: Channels<ChatMessage>,
  mesh: MeshRpc,
  peers: Mutex<BTreeSet<String>>,
}

impl std::fmt::Debug for NodeState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("NodeState")
      .field("node", &self.node)
      .finish_non_exhaustive()
  }
}

impl NodeState {
  /// The state of node `node`: its rooms, their history (`journal`, read through `history`),
  /// its channels and its mesh RPC.
  pub fn new(
    node: impl Into<String>,
    rooms: Rooms,
    history: EventSubscription<Posted>,
    channels: Channels<ChatMessage>,
    mesh: MeshRpc,
  ) -> Self {
    Self {
      node: node.into(),
      journal: rooms.journal(),
      rooms,
      history,
      channels,
      mesh,
      peers: Mutex::new(BTreeSet::new()),
    }
  }

  /// This node's name.
  pub fn node(&self) -> &str {
    &self.node
  }

  /// The channels (rooms' live events).
  pub fn channels(&self) -> &Channels<ChatMessage> {
    &self.channels
  }

  /// The history subscriptions (open readers, for tests and metrics).
  pub fn history(&self) -> &EventSubscription<Posted> {
    &self.history
  }

  /// Remembers `peer` for [`Relay::cluster`] (it stays listed, `up: false`, once gone).
  pub fn add_peer(&self, peer: &str) {
    self.peer_set().insert(peer.to_owned());
  }

  /// The peers this node has had a session with, by name.
  pub fn peers(&self) -> Vec<String> {
    self.peer_set().iter().cloned().collect()
  }

  fn peer_set(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
    self.peers.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// This node's rooms with connections.
  pub fn status(&self) -> NodeStatus {
    let rooms = self
      .channels
      .topics()
      .into_iter()
      .filter_map(|t| {
        let room = room_of(&t)?.to_owned();
        let present = i64::try_from(self.channels.subscribers(&t)).unwrap_or(i64::MAX);
        Some(RoomCount { room, present })
      })
      .collect();
    NodeStatus {
      node: self.node.clone(),
      up: true,
      rooms,
    }
  }

  /// Stores `message` from `who` in its room's history and publishes it to the room: its
  /// acknowledgement.
  fn post(self: Arc<Self>, who: String, message: Outgoing) -> Effect<Ack, RelayError, DynEnv> {
    let Outgoing { room, text } = message;
    if !valid_room(&room) {
      return fail(invalid_room());
    }
    if text.trim().is_empty() || text.chars().count() > MAX_TEXT_LEN {
      return fail(invalid_text(MAX_TEXT_LEN));
    }
    let member = on_dyn(self.rooms.is_member(&room, &who).map_err(internal));
    member.flat_map(move |member| {
      if !member {
        return fail(RelayError::NotAMember { room });
      }
      let posted = Posted {
        from: who.clone(),
        node: self.node.clone(),
        text: text.clone(),
      };
      let stream = topic(&room);
      let appended = on_dyn(
        append(
          self.journal.clone(),
          stream.clone(),
          posted,
          APPEND_ATTEMPTS,
        )
        .map_err(|e| internal(e.to_string())),
      );
      appended.flat_map(move |stored| {
        let Some(seq) = stored.first().map(|e| e.version) else {
          return fail(internal("the journal stored nothing"));
        };
        let seq = i64::try_from(seq).unwrap_or(i64::MAX);
        let live = ChatMessage {
          room: room.clone(),
          seq,
          from: who.clone(),
          node: self.node.clone(),
          text,
        };
        // Stored first: a publish that fails (the room's channel is gone) loses nothing, the
        // history has it.
        if let Err(e) = self.channels.publish(&stream, Some(&who), &live) {
          log_warning(&format!("publish to {stream} failed: {e}"));
        }
        succeed(Ack { room, seq })
      })
    })
  }
}

/// The handlers of [`Relay`] on one node.
#[derive(Clone, Debug)]
pub struct Relays(pub Arc<NodeState>);

/// The handlers of [`RelayPeer`] on one node.
#[derive(Clone, Debug)]
pub struct Peers(pub Arc<NodeState>);

impl RelayPeer for Peers {
  fn status(&self) -> Effect<NodeStatus, RelayError, DynEnv> {
    succeed(self.0.status())
  }
}

impl Relay for Relays {
  fn create_room(&self, room: String) -> Effect<(), RelayError, DynEnv> {
    let rooms = self.0.rooms.clone();
    principal().flat_map(move |who: CurrentPrincipal| {
      if !valid_room(&room) {
        return fail(invalid_room());
      }
      on_dyn(rooms.create(&room, who.id()).map_err(internal)).flat_map(move |created| {
        if created {
          succeed(())
        } else {
          fail(RelayError::RoomExists { room })
        }
      })
    })
  }

  fn invite(&self, invite: Invite) -> Effect<(), RelayError, DynEnv> {
    let rooms = self.0.rooms.clone();
    principal().flat_map(move |who: CurrentPrincipal| {
      let Invite { room, user } = invite;
      if !valid_room(&room) {
        return fail(invalid_room());
      }
      if user.trim().is_empty() || user.chars().count() > MAX_USER_LEN {
        return fail(invalid_text(MAX_USER_LEN));
      }
      let check = on_dyn(rooms.is_member(&room, who.id()).map_err(internal));
      check.flat_map(move |member| {
        if !member {
          return fail(RelayError::NotAMember { room });
        }
        on_dyn(rooms.add_member(&room, &user).map_err(internal))
      })
    })
  }

  fn chat(&self) -> Effect<Duplex<Outgoing, Ack, RelayError>, RelayError, DynEnv> {
    let state = Arc::clone(&self.0);
    principal().map(move |who: CurrentPrincipal| {
      let who = who.id().to_owned();
      Duplex::new(move |items: Stream<Outgoing, RelayError, DynEnv>| {
        items.map_effect(move |message| Arc::clone(&state).post(who.clone(), message))
      })
    })
  }

  fn send(&self, message: Outgoing) -> Effect<Ack, RelayError, DynEnv> {
    let state = Arc::clone(&self.0);
    principal().flat_map(move |who: CurrentPrincipal| state.post(who.id().to_owned(), message))
  }

  fn join(
    &self,
    join: JoinRoom,
  ) -> Effect<Subscription<RoomEvent, RelayError>, RelayError, DynEnv> {
    let JoinRoom { room, status } = join;
    if !valid_room(&room) {
      return fail(invalid_room());
    }
    if status.chars().count() > MAX_STATUS_LEN {
      return fail(invalid_text(MAX_STATUS_LEN));
    }
    let channels = self.0.channels.clone();
    principal().flat_map(move |who: CurrentPrincipal| {
      connection().flat_map(move |conn: Option<String>| {
        let conn = conn.unwrap_or_else(|| channels.fresh_conn());
        let spec = JoinSpec::new(topic(&room), conn).with_meta(json!({ "status": status }));
        let refused = room.clone();
        channels
          .join(spec, who)
          .map_err(move |e| channel_error(&refused, e))
          .map(move |membership| {
            let failed = room.clone();
            let items = membership
              .into_subscription()
              .into_stream()
              .map_err(move |e| channel_error(&failed, e))
              .map(|item| SubscriptionItem::new(room_event(item.value)));
            Subscription::new(items)
          })
      })
    })
  }

  fn history(
    &self,
    room: String,
    after: Option<Cursor>,
  ) -> Effect<Subscription<ChatMessage, RelayError>, RelayError, DynEnv> {
    if !valid_room(&room) {
      return fail(invalid_room());
    }
    let state = Arc::clone(&self.0);
    principal().flat_map(move |who: CurrentPrincipal| {
      on_dyn(state.rooms.is_member(&room, who.id()).map_err(internal)).flat_map(move |member| {
        if !member {
          return fail(RelayError::NotAMember { room });
        }
        let stream = topic(&room);
        let subscribed = state
          .history
          .subscribe(after, move |event: RecordedEvent<Posted>| {
            (event.stream_id == stream).then(|| message_of(&room, &event))
          });
        match subscribed {
          Ok(subscription) => succeed(subscription),
          Err(_) => fail(RelayError::InvalidCursor),
        }
      })
    })
  }

  fn cluster(&self) -> Effect<ClusterView, RelayError, DynEnv> {
    let state = Arc::clone(&self.0);
    let mut view: Effect<Vec<NodeStatus>, RelayError, DynEnv> = succeed(vec![state.status()]);
    for peer in state.peers() {
      let client = RelayPeerService::client(MeshTransport::new(
        state
          .mesh
          .client(peer_service(&peer))
          .with_timeout(PEER_TIMEOUT),
        RelayPeerService::group(Peers(Arc::clone(&state))),
      ));
      let asked: Effect<Exit<NodeStatus, _>, RelayError, DynEnv> = client.status().exit();
      view = view.flat_map(move |mut nodes| {
        asked.map(move |exit| {
          nodes.push(match exit {
            Exit::Success(status) => status,
            Exit::Failure(_) => NodeStatus {
              node: peer,
              up: false,
              rooms: Vec::new(),
            },
          });
          nodes
        })
      });
    }
    view.map(|nodes| ClusterView { nodes })
  }
}

/// How many times a message is appended when other posts to the room win the race for the next
/// number.
pub const APPEND_ATTEMPTS: u32 = 16;

/// Appends `posted` to `stream` at the stream's next version: a post that lost the race to a
/// concurrent one (on any node) is appended again after it, at most `attempts` times.
fn append(
  journal: Journal,
  stream: String,
  posted: Posted,
  attempts: u32,
) -> Effect<Vec<StoredEvent<Posted>>, EventStoreError> {
  journal
    .append(&stream, std::slice::from_ref(&posted))
    .catch(move |e| match e {
      EventStoreError::VersionConflict { .. } if attempts > 1 => {
        append(journal, stream, posted, attempts - 1)
      }
      other => fail(other),
    })
}

/// The history item of a stored event.
fn message_of(room: &str, event: &RecordedEvent<Posted>) -> ChatMessage {
  let posted = &event.event.payload;
  ChatMessage {
    room: room.to_owned(),
    seq: i64::try_from(event.event.version).unwrap_or(i64::MAX),
    from: posted.from.clone(),
    node: posted.node.clone(),
    text: posted.text.clone(),
  }
}

/// A channel event as the Relay's clients see it.
fn room_event(event: ChannelEvent<ChatMessage>) -> RoomEvent {
  let empty = |kind: &str, topic: &str| RoomEvent {
    kind: kind.to_owned(),
    room: room_of(topic).unwrap_or(topic).to_owned(),
    members: Vec::new(),
    joins: Vec::new(),
    leaves: Vec::new(),
    message: None,
  };
  match event {
    ChannelEvent::Joined { topic, members, .. } => RoomEvent {
      members: members.iter().map(member).collect(),
      ..empty("joined", &topic)
    },
    ChannelEvent::Message { topic, body, .. } => RoomEvent {
      message: Some(body),
      ..empty("message", &topic)
    },
    ChannelEvent::Presence { topic, diff } => RoomEvent {
      joins: diff.joins.iter().map(member).collect(),
      leaves: diff.leaves.iter().map(member).collect(),
      ..empty("presence", &topic)
    },
  }
}

fn member(p: &Presence) -> Member {
  Member {
    user: p.key.clone(),
    conn: p.conn.clone(),
    node: p.node.clone(),
    status: p
      .meta
      .get("status")
      .and_then(Value::as_str)
      .unwrap_or_default()
      .to_owned(),
  }
}

/// A channel failure as a Relay failure (the reason stays in the server log).
fn channel_error(room: &str, e: ChannelError) -> RelayError {
  match e {
    ChannelError::JoinDenied | ChannelError::Unauthenticated => RelayError::NotAMember {
      room: room.to_owned(),
    },
    ChannelError::InvalidTopic => invalid_room(),
    ChannelError::MetaTooLarge => invalid_text(MAX_STATUS_LEN),
    ChannelError::MessageTooLarge => invalid_text(MAX_TEXT_LEN),
    ChannelError::Lagged => RelayError::Lagged,
    ChannelError::TopicFull
    | ChannelError::AlreadyJoined
    | ChannelError::TooManyTopics
    | ChannelError::Closed => RelayError::Busy,
    ChannelError::Internal(detail) => RelayError::Internal { detail },
  }
}

fn invalid_room() -> RelayError {
  RelayError::InvalidRoom {
    max: i64::try_from(MAX_ROOM_LEN).unwrap_or(i64::MAX),
  }
}

fn invalid_text(max: usize) -> RelayError {
  RelayError::InvalidText {
    max: i64::try_from(max).unwrap_or(i64::MAX),
  }
}

fn internal(e: impl ToString) -> RelayError {
  RelayError::Internal {
    detail: e.to_string(),
  }
}

impl From<SqlError> for RelayError {
  fn from(e: SqlError) -> Self {
    internal(e)
  }
}

/// A warning on the node's standard error (the reference app logs nothing else).
fn log_warning(message: &str) {
  eprintln!("relay: {message}");
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_kit::rpc::channels::PresenceDiff;

  fn presence(key: &str, status: Value) -> Presence {
    Presence {
      key: key.into(),
      conn: "c1".into(),
      node: "a@relay".into(),
      meta: status,
    }
  }

  #[test]
  fn channel_events_map_to_room_events() {
    let joined = room_event(ChannelEvent::Joined {
      topic: "room:general".into(),
      me: presence("ada", json!({"status": "online"})),
      members: vec![presence("ada", json!({"status": "online"}))],
    });
    assert_eq!(joined.kind, "joined");
    assert_eq!(joined.room, "general");
    assert_eq!(joined.members[0].status, "online");
    let left = room_event(ChannelEvent::Presence {
      topic: "room:general".into(),
      diff: PresenceDiff {
        joins: Vec::new(),
        leaves: vec![presence("bob", Value::Null)],
      },
    });
    assert_eq!(left.kind, "presence");
    assert_eq!(left.leaves[0].user, "bob");
    assert_eq!(left.leaves[0].status, "");
  }

  #[test]
  fn channel_failures_map_to_relay_failures() {
    let room = "general";
    assert_eq!(
      channel_error(room, ChannelError::JoinDenied),
      RelayError::NotAMember { room: room.into() }
    );
    assert_eq!(
      channel_error(room, ChannelError::Lagged),
      RelayError::Lagged
    );
    assert_eq!(channel_error(room, ChannelError::Closed), RelayError::Busy);
    assert!(matches!(
      channel_error(room, ChannelError::InvalidTopic),
      RelayError::InvalidRoom { .. }
    ));
    assert!(matches!(
      channel_error(room, ChannelError::MessageTooLarge),
      RelayError::InvalidText { .. }
    ));
    assert!(matches!(
      channel_error(room, ChannelError::Internal("x".into())),
      RelayError::Internal { .. }
    ));
    assert!(matches!(
      RelayError::from(SqlError::NotConnected),
      RelayError::Internal { .. }
    ));
    assert_eq!(peer_service("a@relay"), "RelayPeer@a@relay");
  }
}
