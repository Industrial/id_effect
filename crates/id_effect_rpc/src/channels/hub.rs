//! [`Channels`]: one node's topics, subscribers and presence, and what its hub does with the
//! frames other hubs send.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

use id_effect::{DynEnv, Effect, Stream, fail, from_async, succeed};
use id_effect_authz::Decision;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use super::backplane::{Backplane, Inbound, MeshBackplane};
use super::clock::{ChannelClock, SystemClock};
use super::policy::{DenyAllJoins, JoinPolicy, JoinRequest};
use super::presence::{Presence, PresenceDiff, PresenceTable, PresenceWire};
use super::{ChannelError, ChannelsConfig, check_topic};
use crate::kinds::{Subscription, SubscriptionItem};

/// What a channel message can be: any serde type (postcard between nodes, JSON to clients).
pub trait ChannelMessage: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {}

impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> ChannelMessage for T {}

/// One item of a topic subscription (JSON: `{"type": "joined" | "message" | "presence", …}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelEvent<M> {
  /// The first item: the join succeeded; who is on the topic now (this connection included).
  Joined {
    /// The topic.
    topic: String,
    /// This connection's own entry.
    me: Presence,
    /// Every member this node knows of.
    members: Vec<Presence>,
  },
  /// A message published on the topic.
  Message {
    /// The topic.
    topic: String,
    /// The node it was published on.
    node: String,
    /// The publisher's own label (a principal id, say), if it gave one.
    from: Option<String>,
    /// The message.
    body: M,
  },
  /// Members joined or left.
  Presence {
    /// The topic.
    topic: String,
    /// What changed.
    #[serde(flatten)]
    diff: PresenceDiff,
  },
}

/// What one hub sends another (postcard, inside the hub process's cast).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum HubFrame {
  /// A message published on `node` (life `epoch`), numbered `seq` in its publish order.
  Publish {
    node: String,
    epoch: u64,
    seq: u64,
    topic: String,
    from: Option<String>,
    payload: Vec<u8>,
  },
  /// Members of `node` joined or left `topic`.
  Presence {
    node: String,
    topic: String,
    joins: Vec<PresenceWire>,
    leaves: Vec<String>,
  },
  /// All members of `node` on `topic` (the heartbeat).
  Members {
    node: String,
    topic: String,
    members: Vec<PresenceWire>,
  },
}

/// The most nodes remembered as down at once.
const MAX_DOWN: usize = 1_024;

/// The most (topic, publisher) positions remembered at once.
const MAX_SEEN: usize = 65_536;

impl HubFrame {
  /// The node that sent the frame.
  fn node(&self) -> &str {
    match self {
      Self::Publish { node, .. } | Self::Presence { node, .. } | Self::Members { node, .. } => node,
    }
  }

  fn encode(&self) -> Vec<u8> {
    postcard::to_allocvec(self).unwrap_or_default()
  }
}

/// Who joins what: the input of [`Channels::join`].
#[derive(Clone, Debug)]
pub struct JoinSpec {
  /// The topic.
  pub topic: String,
  /// The joining connection (unique on this node; the cap on topics counts per connection).
  pub conn: String,
  /// The metadata to join with (`null` for none).
  pub meta: Value,
}

impl JoinSpec {
  /// Join `topic` as connection `conn`, without metadata.
  pub fn new(topic: impl Into<String>, conn: impl Into<String>) -> Self {
    Self {
      topic: topic.into(),
      conn: conn.into(),
      meta: Value::Null,
    }
  }

  /// Join with `meta`.
  #[must_use]
  pub fn with_meta(mut self, meta: Value) -> Self {
    self.meta = meta;
    self
  }
}

/// The outcome of [`Channels::publish`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Published {
  /// Local subscribers it was handed to.
  pub local: usize,
  /// Other nodes it was cast to (their hubs fan it out to their subscribers).
  pub nodes: usize,
}

struct Sub<M> {
  conn: String,
  tx: mpsc::Sender<ChannelEvent<M>>,
  lagged: Arc<AtomicBool>,
}

struct State<M> {
  topics: BTreeMap<String, BTreeMap<u64, Sub<M>>>,
  presence: PresenceTable,
  /// The publisher's life and the last sequence number delivered, per (topic, node).
  seen: BTreeMap<(String, String), (u64, u64)>,
  /// Topics joined per connection.
  conns: HashMap<String, usize>,
  /// Nodes reported down and not seen again: their frames still in flight are ignored, so a
  /// late heartbeat cannot bring a lost node's members back.
  down: BTreeSet<String>,
  next_sub: u64,
  next_seq: u64,
  closed: bool,
}

struct Inner<M> {
  node: String,
  epoch: u64,
  config: ChannelsConfig,
  policy: Arc<dyn JoinPolicy>,
  clock: Arc<dyn ChannelClock>,
  backplane: OnceLock<Arc<dyn Backplane>>,
  state: Mutex<State<M>>,
  /// Publishes are numbered and sent in one order.
  publishing: Mutex<()>,
  next_conn: AtomicU64,
}

/// One node's channels: topics, subscribers, presence, and the hub that links them to the other
/// nodes. Clones share everything.
pub struct Channels<M> {
  inner: Arc<Inner<M>>,
}

impl<M> Clone for Channels<M> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<M> fmt::Debug for Channels<M> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Channels")
      .field("node", &self.inner.node)
      .field("config", &self.inner.config)
      .finish_non_exhaustive()
  }
}

/// Builds [`Channels`]: configuration, join policy, clock, then the backplane.
pub struct ChannelsBuilder<M> {
  config: ChannelsConfig,
  policy: Arc<dyn JoinPolicy>,
  clock: Arc<dyn ChannelClock>,
  namespace: String,
  _message: std::marker::PhantomData<fn() -> M>,
}

impl<M> fmt::Debug for ChannelsBuilder<M> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ChannelsBuilder")
      .field("config", &self.config)
      .field("namespace", &self.namespace)
      .finish_non_exhaustive()
  }
}

impl<M: ChannelMessage> ChannelsBuilder<M> {
  /// Use `config`.
  #[must_use]
  pub fn config(mut self, config: ChannelsConfig) -> Self {
    self.config = config;
    self
  }

  /// Decide joins with `policy` (default [`DenyAllJoins`]).
  #[must_use]
  pub fn join_policy(mut self, policy: impl JoinPolicy) -> Self {
    self.policy = Arc::new(policy);
    self
  }

  /// Measure presence expiry with `clock` (default [`SystemClock`]).
  #[must_use]
  pub fn clock(mut self, clock: impl ChannelClock) -> Self {
    self.clock = Arc::new(clock);
    self
  }

  /// Run these channels under `namespace` (default `"default"`): one hub per namespace and
  /// node, so several sets of channels (message types) can share a mesh.
  #[must_use]
  pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
    self.namespace = namespace.into();
    self
  }

  /// Start on `mesh`: spawn this node's hub process and register it cluster-wide.
  ///
  /// # Errors
  ///
  /// [`ChannelError::Internal`] when the hub behavior was not registered
  /// ([`register_hub`](super::register_hub)) before the mesh was built, or when this namespace
  /// already has a hub on this node.
  pub fn start_on_mesh(self, mesh: &id_effect_node::Mesh) -> Result<Channels<M>, ChannelError> {
    let mesh = mesh.clone();
    let namespace = self.namespace.clone();
    self.start_with(mesh.node_name().to_string(), move |inbound| {
      MeshBackplane::start(&mesh, &namespace, inbound).map(|b| b as Arc<dyn Backplane>)
    })
  }

  /// Start on another [`Backplane`]: `connect` receives the [`Inbound`] handle the backplane
  /// hands frames and node events to. `node` is this node's name.
  ///
  /// # Errors
  ///
  /// Whatever `connect` fails with.
  pub fn start_with(
    self,
    node: impl Into<String>,
    connect: impl FnOnce(Inbound) -> Result<Arc<dyn Backplane>, ChannelError>,
  ) -> Result<Channels<M>, ChannelError> {
    let config = self.config;
    let inner = Arc::new(Inner {
      node: node.into(),
      epoch: rand::random(),
      policy: self.policy,
      clock: self.clock,
      backplane: OnceLock::new(),
      state: Mutex::new(State {
        topics: BTreeMap::new(),
        presence: PresenceTable::new(config.max_members_per_topic()),
        seen: BTreeMap::new(),
        conns: HashMap::new(),
        down: BTreeSet::new(),
        next_sub: 0,
        next_seq: 0,
        closed: false,
      }),
      config,
      publishing: Mutex::new(()),
      next_conn: AtomicU64::new(0),
    });
    let weak: Weak<dyn HubSink> = Arc::downgrade(&(Arc::clone(&inner) as Arc<dyn HubSink>));
    let backplane = connect(Inbound::new(weak))?;
    let _ = inner.backplane.set(backplane);
    Ok(Channels { inner })
  }
}

impl<M: ChannelMessage> Channels<M> {
  /// A builder: default configuration, [`DenyAllJoins`], [`SystemClock`], namespace
  /// `"default"`.
  pub fn builder() -> ChannelsBuilder<M> {
    ChannelsBuilder {
      config: ChannelsConfig::default(),
      policy: Arc::new(DenyAllJoins),
      clock: Arc::new(SystemClock::new()),
      namespace: "default".to_string(),
      _message: std::marker::PhantomData,
    }
  }

  /// This node's name.
  pub fn node(&self) -> &str {
    &self.inner.node
  }

  /// The configuration in force.
  pub fn config(&self) -> &ChannelsConfig {
    &self.inner.config
  }

  /// A fresh connection id, unique on this node (for callers without one of their own).
  pub fn fresh_conn(&self) -> String {
    let n = self.inner.next_conn.fetch_add(1, Ordering::Relaxed);
    format!("{}#{:x}-{n}", self.inner.node, self.inner.epoch)
  }

  /// Join `spec.topic` as `principal`: the join policy decides, then the caps; the membership's
  /// first event is [`ChannelEvent::Joined`]. Dropping the membership leaves.
  ///
  /// # Errors
  ///
  /// [`ChannelError::JoinDenied`] / [`ChannelError::Unauthenticated`] when the policy denies,
  /// the cap errors, [`ChannelError::InvalidTopic`], [`ChannelError::MetaTooLarge`],
  /// [`ChannelError::Closed`]; [`ChannelError::Internal`] when the policy fails.
  pub fn join(
    &self,
    spec: JoinSpec,
    principal: id_effect_authz::CurrentPrincipal,
  ) -> Effect<Membership<M>, ChannelError, DynEnv> {
    if let Err(e) = check_topic(&spec.topic) {
      return fail(e);
    }
    if spec.meta.to_string().len() > self.inner.config.max_meta_bytes() {
      return fail(ChannelError::MetaTooLarge);
    }
    let request = JoinRequest {
      topic: spec.topic.clone(),
      principal: principal.clone(),
      meta: spec.meta.clone(),
    };
    let channels = self.clone();
    self
      .inner
      .policy
      .decide(&request)
      .map_err(|e| ChannelError::Internal(format!("join policy: {e:?}")))
      .flat_map(move |decision| match decision {
        Decision::Allow => match channels.admit(spec, principal.id()) {
          Ok(membership) => succeed(membership),
          Err(e) => fail(e),
        },
        Decision::Deny(denial) => {
          tracing::info!(topic = %spec.topic, reason = ?denial, "channel join denied");
          if principal.is_authenticated() {
            fail(ChannelError::JoinDenied)
          } else {
            fail(ChannelError::Unauthenticated)
          }
        }
      })
  }

  /// Publish `message` on `topic`: to the local subscribers now, and one cast to each other
  /// node with subscribers. `from` labels the publisher in the [`ChannelEvent::Message`].
  /// Publishing is the application's decision: guard the procedure that calls this.
  ///
  /// # Errors
  ///
  /// [`ChannelError::InvalidTopic`], [`ChannelError::MessageTooLarge`],
  /// [`ChannelError::Closed`], [`ChannelError::Internal`] when `message` does not encode.
  pub fn publish(
    &self,
    topic: &str,
    from: Option<&str>,
    message: &M,
  ) -> Result<Published, ChannelError> {
    check_topic(topic)?;
    let payload =
      postcard::to_allocvec(message).map_err(|e| ChannelError::Internal(e.to_string()))?;
    if payload.len() > self.inner.config.max_message_bytes() {
      return Err(ChannelError::MessageTooLarge);
    }
    let _order = self
      .inner
      .publishing
      .lock()
      .unwrap_or_else(PoisonError::into_inner);
    let (seq, local, dropped) = {
      let mut state = self.inner.lock();
      if state.closed {
        return Err(ChannelError::Closed);
      }
      state.next_seq += 1;
      let seq = state.next_seq;
      let event = ChannelEvent::Message {
        topic: topic.to_string(),
        node: self.inner.node.clone(),
        from: from.map(str::to_string),
        body: message.clone(),
      };
      let local = state.topics.get(topic).map_or(0, BTreeMap::len);
      let dropped = deliver(&mut state, topic, &event);
      (seq, local - dropped.len(), dropped)
    };
    let frame = HubFrame::Publish {
      node: self.inner.node.clone(),
      epoch: self.inner.epoch,
      seq,
      topic: topic.to_string(),
      from: from.map(str::to_string),
      payload,
    };
    let nodes = self.inner.backplane().fanout(topic, &frame.encode());
    drop(_order);
    self.inner.leave_all(dropped);
    Ok(Published { local, nodes })
  }

  /// Who is on `topic`, as this node knows it (empty unless this node has subscribers on it).
  pub fn members(&self, topic: &str) -> Vec<Presence> {
    self.inner.lock().presence.list(topic)
  }

  /// The other nodes with subscribers on `topic`, as this node's hub knows them.
  pub fn nodes(&self, topic: &str) -> Vec<String> {
    self.inner.backplane().nodes(topic)
  }

  /// The topics with subscribers on this node.
  pub fn topics(&self) -> Vec<String> {
    self.inner.lock().topics.keys().cloned().collect()
  }

  /// The subscribers of `topic` on this node.
  pub fn subscribers(&self, topic: &str) -> usize {
    self.inner.lock().topics.get(topic).map_or(0, BTreeMap::len)
  }

  /// One heartbeat: lapse the remote members whose node stopped refreshing them, send this
  /// node's members of every topic to the other interested nodes, and let the backplane run its
  /// own anti-entropy (hub discovery, group gossip). [`Channels::spawn_heartbeat`] calls it
  /// every [`ChannelsConfig::heartbeat_interval`]; tests call it under a [`ManualClock`].
  ///
  /// [`ManualClock`]: super::ManualClock
  pub fn heartbeat(&self) {
    self.inner.backplane().heartbeat();
    let now = self.inner.clock.now();
    let (frames, dropped) = {
      let mut state = self.inner.lock();
      let mut dropped = Vec::new();
      for (topic, diff) in state.presence.expire(now) {
        dropped.extend(push_diff(&mut state, &topic, diff));
      }
      let frames: Vec<(String, HubFrame)> = state
        .topics
        .keys()
        .map(|topic| {
          let members = state.presence.of_node(topic, &self.inner.node);
          (topic.clone(), self.inner.members_frame(topic, &members))
        })
        .collect();
      (frames, dropped)
    };
    for (topic, frame) in frames {
      self.inner.backplane().fanout(&topic, &frame.encode());
    }
    self.inner.leave_all(dropped);
  }

  /// Run [`Channels::heartbeat`] every [`ChannelsConfig::heartbeat_interval`] on the current
  /// Tokio runtime until the returned task is dropped.
  ///
  /// # Panics
  ///
  /// Outside a Tokio runtime.
  pub fn spawn_heartbeat(&self) -> HeartbeatTask {
    let weak = Arc::downgrade(&self.inner);
    let every = self.inner.config.heartbeat_interval();
    HeartbeatTask(tokio::spawn(async move {
      let mut tick = tokio::time::interval(every);
      loop {
        tick.tick().await;
        match weak.upgrade() {
          Some(inner) => Channels { inner }.heartbeat(),
          None => break,
        }
      }
    }))
  }

  /// Leave every topic and stop the hub: subscribers end, other nodes drop this node's members.
  pub fn shutdown(&self) {
    let subs: Vec<(String, u64)> = {
      let mut state = self.inner.lock();
      state.closed = true;
      state
        .topics
        .iter()
        .flat_map(|(t, subs)| subs.keys().map(move |id| (t.clone(), *id)))
        .collect()
    };
    self.inner.leave_all(subs);
    self.inner.backplane().shutdown();
  }

  /// Admission after the policy allowed: the caps, then the subscriber and its presence.
  fn admit(&self, spec: JoinSpec, key: &str) -> Result<Membership<M>, ChannelError> {
    let inner = &self.inner;
    let me = Presence {
      key: key.to_string(),
      conn: spec.conn.clone(),
      node: inner.node.clone(),
      meta: spec.meta,
    };
    let topic = spec.topic;
    let (rx, lagged, id, first, dropped) = {
      let mut state = inner.lock();
      if state.closed {
        return Err(ChannelError::Closed);
      }
      if state.conns.get(&spec.conn).copied().unwrap_or(0)
        >= inner.config.max_topics_per_connection()
      {
        return Err(ChannelError::TooManyTopics);
      }
      let first = !state.topics.contains_key(&topic);
      if first && state.topics.len() >= inner.config.max_topics() {
        return Err(ChannelError::TooManyTopics);
      }
      if let Some(subs) = state.topics.get(&topic)
        && subs.values().any(|s| s.conn == spec.conn)
      {
        return Err(ChannelError::AlreadyJoined);
      }
      if state.presence.count(&topic) >= inner.config.max_members_per_topic() {
        return Err(ChannelError::TopicFull);
      }
      state.presence.track(&topic);
      state.presence.insert(&topic, me.clone(), None);
      let (tx, rx) = mpsc::channel(inner.config.subscriber_buffer());
      let joined = ChannelEvent::Joined {
        topic: topic.clone(),
        me: me.clone(),
        members: state.presence.list(&topic),
      };
      let _ = tx.try_send(joined);
      let dropped = push_diff(
        &mut state,
        &topic,
        PresenceDiff {
          joins: vec![me.clone()],
          leaves: Vec::new(),
        },
      );
      state.next_sub += 1;
      let id = state.next_sub;
      let lagged = Arc::new(AtomicBool::new(false));
      state.topics.entry(topic.clone()).or_default().insert(
        id,
        Sub {
          conn: spec.conn.clone(),
          tx,
          lagged: Arc::clone(&lagged),
        },
      );
      *state.conns.entry(spec.conn).or_insert(0) += 1;
      (rx, lagged, id, first, dropped)
    };
    let backplane = inner.backplane();
    if first {
      backplane.set_interest(&topic, true);
    }
    let frame = HubFrame::Presence {
      node: inner.node.clone(),
      topic: topic.clone(),
      joins: vec![PresenceWire::of(&me)],
      leaves: Vec::new(),
    };
    backplane.fanout(&topic, &frame.encode());
    inner.leave_all(dropped);
    Ok(Membership {
      rx,
      lagged,
      _leave: Leave {
        inner: Arc::downgrade(inner),
        topic,
        id,
      },
    })
  }
}

impl<M> Inner<M> {
  fn lock(&self) -> std::sync::MutexGuard<'_, State<M>> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  fn backplane(&self) -> &Arc<dyn Backplane> {
    self.backplane.get().expect("backplane is set at start")
  }

  fn members_frame(&self, topic: &str, members: &[Presence]) -> HubFrame {
    HubFrame::Members {
      node: self.node.clone(),
      topic: topic.to_string(),
      members: members.iter().map(PresenceWire::of).collect(),
    }
  }

  fn expiry(&self) -> Duration {
    self.clock.now() + self.config.presence_ttl()
  }
}

impl<M: ChannelMessage> Inner<M> {
  /// Remove subscribers (and whoever lags while their leaves are pushed), one after the other.
  fn leave_all(&self, mut work: Vec<(String, u64)>) {
    while let Some((topic, id)) = work.pop() {
      work.extend(self.leave(&topic, id));
    }
  }

  /// Remove one subscriber: its presence leaves, and the topic when it was the last one.
  fn leave(&self, topic: &str, id: u64) -> Vec<(String, u64)> {
    let (gone, last, dropped) = {
      let mut state = self.lock();
      let Some(sub) = state
        .topics
        .get_mut(topic)
        .and_then(|subs| subs.remove(&id))
      else {
        return Vec::new();
      };
      if let Some(n) = state.conns.get_mut(&sub.conn) {
        *n -= 1;
        if *n == 0 {
          state.conns.remove(&sub.conn);
        }
      }
      let gone = state.presence.remove(topic, &self.node, &sub.conn);
      let last = state.topics.get(topic).is_some_and(BTreeMap::is_empty);
      let mut dropped = Vec::new();
      if last {
        state.topics.remove(topic);
        state.presence.untrack(topic);
        state.seen.retain(|(t, _), _| t != topic);
      } else if let Some(p) = &gone {
        dropped = push_diff(
          &mut state,
          topic,
          PresenceDiff {
            joins: Vec::new(),
            leaves: vec![p.clone()],
          },
        );
      }
      (gone, last, dropped)
    };
    let backplane = self.backplane();
    if let Some(p) = gone {
      let frame = HubFrame::Presence {
        node: self.node.clone(),
        topic: topic.to_string(),
        joins: Vec::new(),
        leaves: vec![p.conn],
      };
      backplane.fanout(topic, &frame.encode());
    }
    if last {
      backplane.set_interest(topic, false);
    }
    dropped
  }

  fn on_frame(&self, bytes: &[u8]) {
    let frame: HubFrame = match postcard::from_bytes(bytes) {
      Ok(frame) => frame,
      Err(e) => {
        tracing::warn!(error = %e, "channels: undecodable hub frame dropped");
        return;
      }
    };
    let dropped = {
      let mut state = self.lock();
      if state.down.contains(frame.node()) {
        return;
      }
      match frame {
        HubFrame::Publish {
          node,
          epoch,
          seq,
          topic,
          from,
          payload,
        } => {
          if node == self.node || !state.topics.contains_key(&topic) {
            return;
          }
          let key = (topic.clone(), node.clone());
          match state.seen.get(&key) {
            // Same life of the publisher: only what is newer than the last delivered.
            Some(&(life, last)) if life == epoch && seq <= last => return,
            Some(_) => {}
            None if state.seen.len() >= MAX_SEEN => {
              state.seen.pop_first();
            }
            None => {}
          }
          // A new life (the publisher restarted) starts its own numbering.
          state.seen.insert(key, (epoch, seq));
          let body: M = match postcard::from_bytes(&payload) {
            Ok(body) => body,
            Err(e) => {
              tracing::warn!(error = %e, %topic, "channels: undecodable message dropped");
              return;
            }
          };
          let event = ChannelEvent::Message {
            topic: topic.clone(),
            node,
            from,
            body,
          };
          deliver(&mut state, &topic, &event)
        }
        HubFrame::Presence {
          node,
          topic,
          joins,
          leaves,
        } => {
          if node == self.node {
            return;
          }
          let joins = joins.into_iter().map(|w| w.into_presence(&node)).collect();
          let diff = state
            .presence
            .apply(&topic, &node, joins, &leaves, self.expiry());
          push_diff(&mut state, &topic, diff)
        }
        HubFrame::Members {
          node,
          topic,
          members,
        } => {
          if node == self.node {
            return;
          }
          let members = members
            .into_iter()
            .map(|w| w.into_presence(&node))
            .collect();
          let diff = state
            .presence
            .replace_node(&topic, &node, members, self.expiry());
          push_diff(&mut state, &topic, diff)
        }
      }
    };
    self.leave_all(dropped);
  }

  fn on_node_down(&self, node: &str) {
    let dropped = {
      let mut state = self.lock();
      if state.down.len() >= MAX_DOWN {
        state.down.pop_first();
      }
      state.down.insert(node.to_string());
      state.seen.retain(|(_, n), _| n != node);
      let mut dropped = Vec::new();
      for (topic, diff) in state.presence.drop_node(node) {
        dropped.extend(push_diff(&mut state, &topic, diff));
      }
      dropped
    };
    self.leave_all(dropped);
  }

  fn on_node_up(&self, node: &str) {
    self.lock().down.remove(node);
  }

  fn on_retain_nodes(&self, alive: &BTreeSet<String>) {
    let gone: Vec<String> = self
      .lock()
      .presence
      .nodes()
      .into_iter()
      .filter(|n| *n != self.node && !alive.contains(n))
      .collect();
    for node in gone {
      self.on_node_down(&node);
    }
  }

  fn on_peer_interest(&self, topic: &str) {
    let frame = {
      let state = self.lock();
      if !state.topics.contains_key(topic) {
        return;
      }
      self.members_frame(topic, &state.presence.of_node(topic, &self.node))
    };
    self.backplane().fanout(topic, &frame.encode());
  }
}

/// What the backplane calls on the hub (type-erased over the message type).
pub(crate) trait HubSink: Send + Sync {
  fn frame(&self, bytes: &[u8]);
  fn node_down(&self, node: &str);
  fn node_up(&self, node: &str);
  fn retain_nodes(&self, alive: &BTreeSet<String>);
  fn peer_interest(&self, topic: &str);
}

impl<M: ChannelMessage> HubSink for Inner<M> {
  fn frame(&self, bytes: &[u8]) {
    self.on_frame(bytes);
  }

  fn node_down(&self, node: &str) {
    self.on_node_down(node);
  }

  fn node_up(&self, node: &str) {
    self.on_node_up(node);
  }

  fn retain_nodes(&self, alive: &BTreeSet<String>) {
    self.on_retain_nodes(alive);
  }

  fn peer_interest(&self, topic: &str) {
    self.on_peer_interest(topic);
  }
}

/// Push `diff` to the subscribers of `topic`; the subscribers it could not reach.
fn push_diff<M: Clone>(
  state: &mut State<M>,
  topic: &str,
  diff: PresenceDiff,
) -> Vec<(String, u64)> {
  if diff.is_empty() {
    return Vec::new();
  }
  let event = ChannelEvent::Presence {
    topic: topic.to_string(),
    diff,
  };
  deliver(state, topic, &event)
}

/// Hand `event` to every subscriber of `topic`. A full buffer marks the subscriber lagged; it
/// and the subscribers already gone are returned for removal.
fn deliver<M: Clone>(
  state: &mut State<M>,
  topic: &str,
  event: &ChannelEvent<M>,
) -> Vec<(String, u64)> {
  let Some(subs) = state.topics.get(topic) else {
    return Vec::new();
  };
  let mut dropped = Vec::new();
  for (id, sub) in subs {
    match sub.tx.try_send(event.clone()) {
      Ok(()) => {}
      Err(mpsc::error::TrySendError::Full(_)) => {
        sub.lagged.store(true, Ordering::SeqCst);
        dropped.push((topic.to_string(), *id));
      }
      Err(mpsc::error::TrySendError::Closed(_)) => dropped.push((topic.to_string(), *id)),
    }
  }
  dropped
}

/// Leaves the topic when the membership is dropped.
struct Leave<M: ChannelMessage> {
  inner: Weak<Inner<M>>,
  topic: String,
  id: u64,
}

impl<M: ChannelMessage> Drop for Leave<M> {
  fn drop(&mut self) {
    if let Some(inner) = self.inner.upgrade() {
      let dropped = inner.leave(&self.topic, self.id);
      inner.leave_all(dropped);
    }
  }
}

/// A joined connection: its events, in order. Dropping it leaves the topic.
pub struct Membership<M: ChannelMessage> {
  rx: mpsc::Receiver<ChannelEvent<M>>,
  lagged: Arc<AtomicBool>,
  _leave: Leave<M>,
}

impl<M: ChannelMessage> fmt::Debug for Membership<M> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Membership")
      .field("topic", &self._leave.topic)
      .finish_non_exhaustive()
  }
}

impl<M: ChannelMessage> Membership<M> {
  /// The topic.
  pub fn topic(&self) -> &str {
    &self._leave.topic
  }

  /// The next event. `Err(Lagged)` once the subscriber was dropped for falling behind,
  /// `Err(Closed)` when the channels shut down.
  ///
  /// # Errors
  ///
  /// [`ChannelError::Lagged`], [`ChannelError::Closed`].
  pub async fn recv(&mut self) -> Result<ChannelEvent<M>, ChannelError> {
    match self.rx.recv().await {
      Some(event) => Ok(event),
      None => Err(self.ended()),
    }
  }

  /// The next event if one is buffered (`Ok(None)` when none is).
  ///
  /// # Errors
  ///
  /// As [`Membership::recv`].
  pub fn try_recv(&mut self) -> Result<Option<ChannelEvent<M>>, ChannelError> {
    match self.rx.try_recv() {
      Ok(event) => Ok(Some(event)),
      Err(mpsc::error::TryRecvError::Empty) => Ok(None),
      Err(mpsc::error::TryRecvError::Disconnected) => Err(self.ended()),
    }
  }

  fn ended(&self) -> ChannelError {
    if self.lagged.load(Ordering::SeqCst) {
      ChannelError::Lagged
    } else {
      ChannelError::Closed
    }
  }

  /// The events as a subscription stream (no cursors: channels are live). The stream fails with
  /// [`ChannelError::Lagged`] / [`ChannelError::Closed`] as [`Membership::recv`] does; dropping
  /// or interrupting it leaves the topic.
  pub fn into_subscription(self) -> Subscription<ChannelEvent<M>, ChannelError> {
    let shared = Arc::new(tokio::sync::Mutex::new(self));
    Subscription::new(Stream::from_pull(move |max: usize| {
      let shared = Arc::clone(&shared);
      from_async(move || async move {
        let mut membership = shared.lock().await;
        let first = membership.recv().await?;
        let mut batch = vec![SubscriptionItem::new(first)];
        while batch.len() < max {
          match membership.rx.try_recv() {
            Ok(event) => batch.push(SubscriptionItem::new(event)),
            Err(_) => break,
          }
        }
        Ok(Some(batch))
      })
    }))
  }
}

/// The task [`Channels::spawn_heartbeat`] runs; dropping it stops the heartbeat.
#[derive(Debug)]
pub struct HeartbeatTask(tokio::task::JoinHandle<()>);

impl Drop for HeartbeatTask {
  fn drop(&mut self) {
    self.0.abort();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::channels::{ManualClock, join_policy_fn};
  use id_effect_authz::{BasicPrincipal, CurrentPrincipal};

  /// A backplane that records what the hub asks of it.
  #[derive(Default)]
  struct Recorder {
    interest: Mutex<Vec<(String, bool)>>,
    sent: Mutex<Vec<(String, HubFrame)>>,
  }

  impl Backplane for Recorder {
    fn set_interest(&self, topic: &str, interested: bool) {
      self
        .interest
        .lock()
        .unwrap()
        .push((topic.into(), interested));
    }
    fn fanout(&self, topic: &str, frame: &[u8]) -> usize {
      let frame = postcard::from_bytes(frame).unwrap();
      self.sent.lock().unwrap().push((topic.into(), frame));
      1
    }
    fn nodes(&self, _: &str) -> Vec<String> {
      vec!["n2".into()]
    }
    fn heartbeat(&self) {}
    fn shutdown(&self) {}
  }

  fn start() -> (Channels<String>, Arc<Recorder>, Inbound) {
    let recorder = Arc::new(Recorder::default());
    let mut inbound = None;
    let backplane = Arc::clone(&recorder);
    let channels = Channels::<String>::builder()
      .join_policy(join_policy_fn(|_| Decision::Allow))
      .clock(ManualClock::new())
      .start_with("n1", |i| {
        inbound = Some(i);
        Ok(backplane as Arc<dyn Backplane>)
      })
      .unwrap();
    (channels, recorder, inbound.unwrap())
  }

  fn joined(channels: &Channels<String>, topic: &str, conn: &str) -> Membership<String> {
    let principal = CurrentPrincipal::new(BasicPrincipal::new(conn));
    let mut m = channels
      .join(JoinSpec::new(topic, conn), principal)
      .provide_all(DynEnv::new())
      .block_on()
      .into_result()
      .unwrap();
    assert!(matches!(
      m.try_recv(),
      Ok(Some(ChannelEvent::Joined { .. }))
    ));
    m
  }

  fn publish(seq: u64, topic: &str, payload: Vec<u8>) -> Vec<u8> {
    HubFrame::Publish {
      node: "n2".into(),
      epoch: 9,
      seq,
      topic: topic.into(),
      from: None,
      payload,
    }
    .encode()
  }

  fn body(text: &str) -> Vec<u8> {
    postcard::to_allocvec(&text.to_string()).unwrap()
  }

  fn wire(conn: &str) -> PresenceWire {
    PresenceWire {
      key: conn.into(),
      conn: conn.into(),
      meta: "null".into(),
    }
  }

  #[test]
  fn start_with_reports_the_backplanes_error() {
    let err = Channels::<String>::builder()
      .start_with("n1", |_| Err(ChannelError::Internal("no network".into())))
      .unwrap_err();
    assert_eq!(err, ChannelError::Internal("no network".into()));
  }

  #[test]
  fn inbound_frames_are_ordered_deduplicated_and_checked() {
    let (channels, _, inbound) = start();
    let mut m = joined(&channels, "t", "c1");
    inbound.frame(b"\xffnot a frame");
    inbound.frame(&publish(1, "elsewhere", body("x")));
    inbound.frame(&publish(2, "t", body("two")));
    inbound.frame(&publish(1, "t", body("late")));
    inbound.frame(&publish(2, "t", body("duplicate")));
    inbound.frame(&publish(3, "t", vec![0xff, 0xff, 0xff]));
    let own = HubFrame::Publish {
      node: "n1".into(),
      epoch: 1,
      seq: 9,
      topic: "t".into(),
      from: None,
      payload: body("echo"),
    };
    inbound.frame(&own.encode());
    let Ok(Some(ChannelEvent::Message { body, node, .. })) = m.try_recv() else {
      panic!("one message")
    };
    assert_eq!((body.as_str(), node.as_str()), ("two", "n2"));
    assert!(matches!(m.try_recv(), Ok(None)), "nothing else");
    // n2 restarted: its new life numbers from 1 again.
    let reborn = HubFrame::Publish {
      node: "n2".into(),
      epoch: 10,
      seq: 1,
      topic: "t".into(),
      from: Some("n2-user".into()),
      payload: postcard::to_allocvec(&"again".to_string()).unwrap(),
    };
    inbound.frame(&reborn.encode());
    assert!(matches!(
      m.try_recv(),
      Ok(Some(ChannelEvent::Message { from: Some(_), .. }))
    ));
  }

  #[test]
  fn presence_frames_node_down_and_back_up() {
    let (channels, recorder, inbound) = start();
    let mut m = joined(&channels, "t", "c1");
    let presence = |node: &str, joins, leaves| {
      HubFrame::Presence {
        node: node.into(),
        topic: "t".into(),
        joins,
        leaves,
      }
      .encode()
    };
    inbound.frame(&presence("n1", vec![wire("self")], vec![]));
    inbound.frame(&presence("n2", vec![wire("r1")], vec![]));
    assert!(matches!(
      m.try_recv(),
      Ok(Some(ChannelEvent::Presence { .. }))
    ));
    let members = HubFrame::Members {
      node: "n2".into(),
      topic: "t".into(),
      members: vec![wire("r2")],
    };
    inbound.frame(&members.encode());
    let own = HubFrame::Members {
      node: "n1".into(),
      topic: "t".into(),
      members: vec![],
    };
    inbound.frame(&own.encode());
    let Ok(Some(ChannelEvent::Presence { diff, .. })) = m.try_recv() else {
      panic!("replace diff")
    };
    assert_eq!((diff.joins.len(), diff.leaves.len()), (1, 1));
    assert_eq!(channels.members("t").len(), 2);
    // Down: n2's members go, and its frames in flight are ignored until it is seen again.
    inbound.node_down("n2");
    assert!(matches!(
      m.try_recv(),
      Ok(Some(ChannelEvent::Presence { .. }))
    ));
    inbound.frame(&presence("n2", vec![wire("r3")], vec![]));
    assert_eq!(channels.members("t").len(), 1);
    inbound.node_up("n2");
    inbound.frame(&presence("n2", vec![wire("r3")], vec![]));
    assert_eq!(channels.members("t").len(), 2);
    // n2's name is still known: its members stay; once it is gone, they go.
    inbound.retain_nodes(&BTreeSet::from(["n2".to_string()]));
    assert_eq!(channels.members("t").len(), 2);
    inbound.retain_nodes(&BTreeSet::new());
    assert_eq!(channels.members("t").len(), 1);
    assert!(matches!(
      m.try_recv(),
      Ok(Some(ChannelEvent::Presence { .. }))
    ));
    inbound.node_up("n2");
    inbound.frame(&presence("n2", vec![wire("r3")], vec![]));
    // A newly interested node gets this node's members; an unknown topic sends nothing.
    let before = recorder.sent.lock().unwrap().len();
    inbound.peer_interest("t");
    inbound.peer_interest("unknown");
    let sent = recorder.sent.lock().unwrap();
    assert_eq!(sent.len(), before + 1);
    assert!(matches!(&sent[before].1, HubFrame::Members { members, .. } if members.len() == 1));
    assert_eq!(channels.nodes("t"), ["n2"]);
  }

  #[test]
  fn interest_follows_the_first_join_and_the_last_leave() {
    let (channels, recorder, _) = start();
    let a = joined(&channels, "t", "c1");
    let b = joined(&channels, "t", "c2");
    drop(a);
    drop(b);
    assert_eq!(
      *recorder.interest.lock().unwrap(),
      [("t".to_string(), true), ("t".to_string(), false)]
    );
    assert!(format!("{channels:?}").contains("n1"));
    let m = joined(&channels, "t", "c3");
    assert!(format!("{m:?}").contains("Membership"));
    assert!(format!("{:?}", JoinSpec::new("t", "c")).contains("JoinSpec"));
  }

  #[test]
  fn down_nodes_are_remembered_within_a_bound() {
    let (channels, _, inbound) = start();
    for n in 0..=MAX_DOWN {
      inbound.node_down(&format!("gone{n}"));
    }
    assert_eq!(channels.inner.lock().down.len(), MAX_DOWN);
    drop(channels);
    // The hub is gone: the inbound handle is inert.
    inbound.frame(&publish(1, "t", body("x")));
    inbound.node_down("n2");
    inbound.node_up("n2");
    inbound.retain_nodes(&BTreeSet::new());
    inbound.peer_interest("t");
    assert!(format!("{inbound:?}").contains("false"));
  }
}
