//! Resume after a reconnect (design note §6): sessions, per-session replay buffers and the
//! decoders of what a client sends to resume.
//!
//! Every connection of a frame transport opens a session: a 128-bit random [`SessionId`] bound
//! to the connection's authenticated principal (C8). The `item`s a resumable call sends (server
//! streams and two-way streams) carry a [`Cursor::sequence`] — the session's message sequence —
//! and stay in the session's replay buffer until the client grants a credit for them (it has
//! them) or a bound evicts them. A dropped connection detaches its session: the resumable calls
//! keep running (their producers suspend once the credits are spent) and the session waits
//! [`ResumeConfig::retention`] for a `resume`. A `resume` on a new connection names the session
//! and, per call, the last cursor the client saw; the server replays what follows it, or answers
//! that call's `exit` with `ResumeExpired` ([`ResumeError::Expired`]) when the buffer no longer
//! holds it — the client then restarts the stream.
//!
//! **Bounds.** Per session at most [`ResumeConfig::max_messages`] buffered frames (default
//! 1,024) and [`ResumeConfig::max_bytes`] of encoded frames (1 MiB), none older than
//! [`ResumeConfig::retention`] (30 s): the oldest go first. At most
//! [`ResumeConfig::max_detached`] sessions (1,024) wait for a resume; a detached session is
//! released after the retention, and its calls are interrupted (finalizers run).
//!
//! **Authority.** A cursor is not a capability: it names a position. A resume is accepted only
//! for a live session id whose principal is the resuming connection's; anything else is answered
//! exactly like an expired buffer (nothing tells a prober whether the session exists), and a
//! principal mismatch is counted ([`ResumeStats::refused_principal`]). Every resumed call runs
//! its procedure's guards again on the new connection's request.
//!
//! ```rust
//! use id_effect_rpc::resume::{self, ResumeError};
//! use id_effect_rpc::wire::Cursor;
//!
//! let text = Cursor::sequence(7).to_string();
//! assert_eq!(resume::decode_cursor(&text), Ok(Cursor::sequence(7)));
//! // An event-log cursor names a subscription position, not a session message.
//! assert_eq!(
//!   resume::decode_cursor(&Cursor::events(7).to_string()),
//!   Err(ResumeError::NotSequence)
//! );
//! assert!(resume::decode_cursor(&"c".repeat(65)).is_err());
//! ```

// The store's internals serve the frame transports (feature `ws`, which WebTransport implies).
#![cfg_attr(not(feature = "ws"), allow(dead_code))]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::task::AbortHandle;

use crate::wire::{CallId, Cursor, CursorError, CursorSource, Frame, Resume, SessionId};

/// Default cap on the frames one session buffers.
pub const DEFAULT_MAX_MESSAGES: usize = 1024;

/// Default cap on the encoded bytes one session buffers.
pub const DEFAULT_MAX_BYTES: usize = 1024 * 1024;

/// Default time a buffered frame and a detached session are kept.
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(30);

/// Default cap on the sessions waiting for a resume at once.
pub const DEFAULT_MAX_DETACHED: usize = 1024;

/// The most calls one `resume` frame may name.
pub const MAX_RESUME_CALLS: usize = 1024;

const TARGET: &str = "id_effect_rpc::resume";

/// Why a resume (or one call of it) cannot go ahead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResumeError {
  /// The buffer no longer holds what follows the cursor (or the session or call is unknown):
  /// the call's `exit` is `ResumeExpired` and the client restarts the stream.
  #[error("resume expired: the replay buffer no longer holds the cursor's successors")]
  Expired,
  /// The cursor text does not decode.
  #[error(transparent)]
  Cursor(#[from] CursorError),
  /// A cursor of another source (an event-log position) where a session sequence belongs.
  #[error("not a session sequence cursor")]
  NotSequence,
  /// The `resume` payload is not a resume object.
  #[error("malformed resume: {0}")]
  Malformed(String),
}

/// Decode a session sequence cursor ([`Cursor::sequence`]): at most
/// [`MAX_CURSOR_LEN`](crate::wire::MAX_CURSOR_LEN) bytes, the `c1.` prefix, base64url without
/// padding, source 2, nothing trailing.
pub fn decode_cursor(text: &str) -> Result<Cursor, ResumeError> {
  let cursor = Cursor::decode(text)?;
  if cursor.source != CursorSource::Sequence {
    return Err(ResumeError::NotSequence);
  }
  Ok(cursor)
}

/// Decode the payload of a `resume` frame: the session, and at most [`MAX_RESUME_CALLS`]
/// distinct call ids (none `0`), each with a sequence cursor.
pub fn decode_resume(payload: &Value) -> Result<Resume, ResumeError> {
  let resume: Resume = serde::Deserialize::deserialize(payload)
    .map_err(|e: serde_json::Error| ResumeError::Malformed(e.to_string()))?;
  if resume.calls.len() > MAX_RESUME_CALLS {
    return Err(ResumeError::Malformed(format!(
      "more than {MAX_RESUME_CALLS} calls"
    )));
  }
  let mut seen = HashSet::with_capacity(resume.calls.len());
  for call in &resume.calls {
    if call.id == CallId::CONNECTION || !seen.insert(call.id) {
      return Err(ResumeError::Malformed(format!(
        "call id {} is the connection or named twice",
        call.id.get()
      )));
    }
    if call.cursor.source != CursorSource::Sequence {
      return Err(ResumeError::NotSequence);
    }
  }
  Ok(resume)
}

/// The time retention is measured in: [`SystemClock`] in production, [`TestClock`] in tests.
pub trait ResumeClock: Send + Sync + 'static {
  /// Time since the clock's origin; never goes backwards.
  fn now(&self) -> Duration;
}

/// Monotonic wall-clock time since the clock was created.
#[derive(Clone, Copy, Debug)]
pub struct SystemClock {
  origin: Instant,
}

impl SystemClock {
  /// A clock starting now.
  pub fn new() -> Self {
    Self {
      origin: Instant::now(),
    }
  }
}

impl Default for SystemClock {
  fn default() -> Self {
    Self::new()
  }
}

impl ResumeClock for SystemClock {
  fn now(&self) -> Duration {
    self.origin.elapsed()
  }
}

/// A clock that moves only when told to (millisecond resolution); clones share the time.
///
/// ```rust
/// use std::time::Duration;
/// use id_effect_rpc::resume::{ResumeClock, TestClock};
///
/// let clock = TestClock::new();
/// clock.advance(Duration::from_secs(31));
/// assert_eq!(clock.clone().now(), Duration::from_secs(31));
/// ```
#[derive(Clone, Debug, Default)]
pub struct TestClock {
  millis: Arc<AtomicU64>,
}

impl TestClock {
  /// A clock at zero.
  pub fn new() -> Self {
    Self::default()
  }

  /// Move the clock forward by `by`.
  pub fn advance(&self, by: Duration) {
    let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
    self.millis.fetch_add(millis, Ordering::SeqCst);
  }
}

impl ResumeClock for TestClock {
  fn now(&self) -> Duration {
    Duration::from_millis(self.millis.load(Ordering::SeqCst))
  }
}

/// The bounds of the replay buffers and sessions; [`Default`] is the design note's.
#[derive(Clone)]
pub struct ResumeConfig {
  max_messages: usize,
  max_bytes: usize,
  retention: Duration,
  max_detached: usize,
  clock: Arc<dyn ResumeClock>,
}

impl Default for ResumeConfig {
  fn default() -> Self {
    Self {
      max_messages: DEFAULT_MAX_MESSAGES,
      max_bytes: DEFAULT_MAX_BYTES,
      retention: DEFAULT_RETENTION,
      max_detached: DEFAULT_MAX_DETACHED,
      clock: Arc::new(SystemClock::new()),
    }
  }
}

impl fmt::Debug for ResumeConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ResumeConfig")
      .field("max_messages", &self.max_messages)
      .field("max_bytes", &self.max_bytes)
      .field("retention", &self.retention)
      .field("max_detached", &self.max_detached)
      .finish_non_exhaustive()
  }
}

impl ResumeConfig {
  /// The defaults: 1,024 messages, 1 MiB, 30 s, 1,024 detached sessions.
  pub fn new() -> Self {
    Self::default()
  }

  /// Buffer at most `messages` frames per session (`0`: buffer nothing, every resume expires).
  #[must_use]
  pub fn max_messages(mut self, messages: usize) -> Self {
    self.max_messages = messages;
    self
  }

  /// Buffer at most `bytes` of encoded frames per session.
  #[must_use]
  pub fn max_bytes(mut self, bytes: usize) -> Self {
    self.max_bytes = bytes;
    self
  }

  /// Keep buffered frames and detached sessions this long (at least 1 ms).
  #[must_use]
  pub fn retention(mut self, retention: Duration) -> Self {
    self.retention = retention.max(Duration::from_millis(1));
    self
  }

  /// At most `sessions` (at least 1) detached sessions wait for a resume; beyond it the one
  /// detached longest is released.
  #[must_use]
  pub fn max_detached(mut self, sessions: usize) -> Self {
    self.max_detached = sessions.max(1);
    self
  }

  /// Measure retention with `clock` (tests: [`TestClock`]).
  #[must_use]
  pub fn clock(mut self, clock: impl ResumeClock) -> Self {
    self.clock = Arc::new(clock);
    self
  }

  /// The per-session frame cap.
  pub fn messages_limit(&self) -> usize {
    self.max_messages
  }

  /// The per-session byte cap.
  pub fn bytes_limit(&self) -> usize {
    self.max_bytes
  }

  /// How long frames and detached sessions are kept.
  pub fn retention_period(&self) -> Duration {
    self.retention
  }

  /// The detached-session cap.
  pub fn detached_limit(&self) -> usize {
    self.max_detached
  }

  fn now(&self) -> Duration {
    self.clock.now()
  }
}

/// What a [`ResumeStore`] holds right now, and what it refused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResumeStats {
  /// Sessions held, attached or not.
  pub sessions: usize,
  /// Sessions waiting for a resume.
  pub detached: usize,
  /// Resumable calls held.
  pub calls: usize,
  /// Frames buffered, over every session.
  pub messages: usize,
  /// Encoded bytes buffered, over every session.
  pub bytes: usize,
  /// Resumes refused because the session belongs to another principal.
  pub refused_principal: u64,
  /// Resumed calls answered `ResumeExpired`.
  pub expired: u64,
}

/// One buffered frame.
struct Entry {
  seq: u64,
  frame: Frame,
  bytes: usize,
  at: Duration,
}

/// The frames of one resumable call, and the transport's state of it (`T`).
struct Record<T> {
  ext: T,
  /// Items not yet credited by the client, oldest first.
  items: VecDeque<Entry>,
  /// The call's `exit`, once it ended.
  exit: Option<Entry>,
  /// The highest sequence no longer buffered (credited or evicted): a cursor below it expired.
  dropped_through: u64,
  /// The sequence of the newest item.
  last_item: u64,
  /// Items recorded.
  total: u64,
  /// Items the client has (credited, or at or before a resume cursor).
  acked: u64,
  abort: Option<AbortHandle>,
}

impl<T> Drop for Record<T> {
  fn drop(&mut self) {
    // The call's task holds its session; a released record interrupts it.
    if let Some(abort) = self.abort.take() {
      abort.abort();
    }
  }
}

struct State<T> {
  next_seq: u64,
  attached: usize,
  detached_at: Option<Duration>,
  calls: HashMap<CallId, Record<T>>,
  /// Every buffered frame by sequence, for evicting the oldest first.
  order: BTreeMap<u64, CallId>,
  messages: usize,
  bytes: usize,
}

impl<T> State<T> {
  fn drop_entry(&mut self, entry: &Entry) {
    self.order.remove(&entry.seq);
    self.messages = self.messages.saturating_sub(1);
    self.bytes = self.bytes.saturating_sub(entry.bytes);
  }

  /// Subtract a removed record's frames.
  fn forget(&mut self, record: &Record<T>) {
    for entry in record.items.iter().chain(record.exit.as_ref()) {
      self.drop_entry(entry);
    }
  }

  /// Evict the oldest buffered frame; `false` when none is.
  fn evict_oldest(&mut self) -> bool {
    let Some((&seq, &call)) = self.order.first_key_value() else {
      return false;
    };
    let Some(record) = self.calls.get_mut(&call) else {
      self.order.remove(&seq);
      return true;
    };
    if record.items.front().is_some_and(|e| e.seq == seq) {
      let entry = record.items.pop_front().expect("front checked");
      record.dropped_through = record.dropped_through.max(seq);
      self.drop_entry(&entry);
    } else if let Some(record) = self.calls.remove(&call) {
      // The exit is a call's last frame: with it goes the call.
      self.forget(&record);
    }
    true
  }

  fn enforce(&mut self, config: &ResumeConfig, now: Duration) {
    while (self.messages > config.max_messages || self.bytes > config.max_bytes)
      && self.evict_oldest()
    {}
    self.expire_aged(config, now);
  }

  fn expire_aged(&mut self, config: &ResumeConfig, now: Duration) {
    while let Some((&seq, &call)) = self.order.first_key_value() {
      let at = self.calls.get(&call).and_then(|r| {
        r.items
          .front()
          .filter(|e| e.seq == seq)
          .or(r.exit.as_ref().filter(|e| e.seq == seq))
          .map(|e| e.at)
      });
      if at.is_some_and(|at| now.saturating_sub(at) < config.retention) {
        break;
      }
      self.evict_oldest();
    }
  }

  /// Free the items of `call` the client has (`acked`).
  fn prune(&mut self, call: CallId) {
    let Some(record) = self.calls.get_mut(&call) else {
      return;
    };
    let mut freed = Vec::new();
    while !record.items.is_empty() && record.total - (record.items.len() as u64) < record.acked {
      let entry = record.items.pop_front().expect("not empty");
      record.dropped_through = record.dropped_through.max(entry.seq);
      freed.push(entry);
    }
    for entry in &freed {
      self.drop_entry(entry);
    }
  }
}

/// One resume session: its id, principal and replay buffer.
pub struct Session<T> {
  id: SessionId,
  principal: Option<String>,
  config: ResumeConfig,
  state: Mutex<State<T>>,
}

impl<T> fmt::Debug for Session<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Session")
      .field("id", &self.id)
      .finish_non_exhaustive()
  }
}

/// What a resume replays for one call.
#[derive(Debug)]
pub(crate) struct Replay {
  /// The buffered frames after the cursor (items, then the exit if the call ended).
  pub frames: Vec<Frame>,
  /// Items at or before the cursor not yet credited: the client has them, so they count as
  /// credited now (the client does not credit them again).
  pub credited: u64,
}

fn lock<S>(m: &Mutex<S>) -> MutexGuard<'_, S> {
  m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<T> Session<T> {
  /// The session id.
  pub fn id(&self) -> SessionId {
    self.id
  }

  /// The principal the session is bound to (`None`: anonymous).
  pub fn principal(&self) -> Option<&str> {
    self.principal.as_deref()
  }

  /// Start buffering `call` (replacing a finished call of the same id).
  pub(crate) fn insert(&self, call: CallId, ext: T) {
    let mut state = lock(&self.state);
    let old = state.calls.remove(&call);
    if let Some(old) = &old {
      state.forget(old);
    }
    state.calls.insert(
      call,
      Record {
        ext,
        items: VecDeque::new(),
        exit: None,
        dropped_through: 0,
        last_item: 0,
        total: 0,
        acked: 0,
        abort: None,
      },
    );
    drop(state);
    drop(old);
  }

  /// The task to interrupt when `call`'s record is released.
  pub(crate) fn set_abort(&self, call: CallId, abort: AbortHandle) {
    match lock(&self.state).calls.get_mut(&call) {
      Some(record) => record.abort = Some(abort),
      None => abort.abort(),
    }
  }

  /// Drop `call`'s record (interrupting its task).
  pub(crate) fn remove(&self, call: CallId) {
    let mut state = lock(&self.state);
    let record = state.calls.remove(&call);
    if let Some(record) = &record {
      state.forget(record);
    }
    drop(state);
    drop(record);
  }

  /// Number `frame` (an `item` of `call`) with the next sequence cursor, encode it with
  /// `encode` (the encoded form and its size) and buffer it. `None` when the call is no longer
  /// held (released): the caller stops.
  pub(crate) fn record_item<M, E>(
    &self,
    call: CallId,
    mut frame: Frame,
    encode: impl FnOnce(&Frame) -> Result<(M, usize), E>,
  ) -> Option<Result<M, E>> {
    let now = self.config.now();
    let mut state = lock(&self.state);
    if !state.calls.contains_key(&call) {
      return None;
    }
    state.next_seq += 1;
    let seq = state.next_seq;
    frame.cursor = Some(Cursor::sequence(seq));
    let (encoded, bytes) = match encode(&frame) {
      Ok(ok) => ok,
      Err(err) => return Some(Err(err)),
    };
    let record = state.calls.get_mut(&call).expect("checked");
    record.total += 1;
    record.last_item = seq;
    record.items.push_back(Entry {
      seq,
      frame,
      bytes,
      at: now,
    });
    state.order.insert(seq, call);
    state.messages += 1;
    state.bytes += bytes;
    state.enforce(&self.config, now);
    Some(Ok(encoded))
  }

  /// Buffer `call`'s `exit` frame (`bytes` encoded). `false` when the call is no longer held.
  pub(crate) fn record_exit(&self, call: CallId, frame: Frame, bytes: usize) -> bool {
    let now = self.config.now();
    let mut state = lock(&self.state);
    if !state.calls.contains_key(&call) {
      return false;
    }
    state.next_seq += 1;
    let seq = state.next_seq;
    let record = state.calls.get_mut(&call).expect("checked");
    record.exit = Some(Entry {
      seq,
      frame,
      bytes,
      at: now,
    });
    state.order.insert(seq, call);
    state.messages += 1;
    state.bytes += bytes;
    state.enforce(&self.config, now);
    true
  }

  /// The client granted `credits` for `call`: it has that many more items. `accept` decides,
  /// under the session's lock (so in order with [`Session::replay`]), whether the grant counts;
  /// returns whether it did.
  pub(crate) fn ack(&self, call: CallId, credits: u64, accept: impl FnOnce() -> bool) -> bool {
    let mut state = lock(&self.state);
    if !accept() {
      return false;
    }
    if let Some(record) = state.calls.get_mut(&call) {
      record.acked = record.acked.saturating_add(credits).min(record.total);
      state.prune(call);
    }
    true
  }

  /// The transport's state of `call`, if it is held.
  pub(crate) fn ext(&self, call: CallId) -> Option<T>
  where
    T: Clone,
  {
    lock(&self.state).calls.get(&call).map(|r| r.ext.clone())
  }

  /// What follows `cursor` for `call`, or [`ResumeError::Expired`] when the buffer no longer
  /// holds all of it (or the cursor names a position the call never reached). `claim` runs
  /// first, under the session's lock: grants [`Session::ack`] has not counted yet no longer
  /// count after it.
  pub(crate) fn replay(
    &self,
    call: CallId,
    cursor: Cursor,
    claim: impl FnOnce(),
  ) -> Result<Replay, ResumeError> {
    let now = self.config.now();
    let mut state = lock(&self.state);
    claim();
    state.expire_aged(&self.config, now);
    let record = state.calls.get_mut(&call).ok_or(ResumeError::Expired)?;
    let position = cursor.position;
    if cursor.source != CursorSource::Sequence
      || position < record.dropped_through
      || position > record.last_item
    {
      return Err(ResumeError::Expired);
    }
    let after = record.items.iter().filter(|e| e.seq > position).count() as u64;
    let have = record.total - after;
    let credited = have.saturating_sub(record.acked);
    record.acked = record.acked.max(have);
    state.prune(call);
    let record = state.calls.get(&call).expect("present");
    let frames = record
      .items
      .iter()
      .chain(record.exit.as_ref())
      .map(|e| e.frame.clone())
      .collect();
    Ok(Replay { frames, credited })
  }

  fn release_all(&self) -> Vec<Record<T>> {
    let mut state = lock(&self.state);
    state.order.clear();
    state.messages = 0;
    state.bytes = 0;
    state.calls.drain().map(|(_, r)| r).collect()
  }
}

struct Sessions<T> {
  map: HashMap<SessionId, Arc<Session<T>>>,
  /// Detach events, oldest first (stale ones are skipped).
  detached: VecDeque<(Duration, SessionId)>,
  detached_count: usize,
}

struct Inner<T> {
  config: ResumeConfig,
  sessions: Mutex<Sessions<T>>,
  refused_principal: AtomicU64,
  expired: AtomicU64,
}

/// The sessions of one transport endpoint (shared by its connections), with their replay
/// buffers. `T` is the transport's state of a resumable call.
pub struct ResumeStore<T> {
  inner: Arc<Inner<T>>,
}

impl<T> Clone for ResumeStore<T> {
  fn clone(&self) -> Self {
    Self {
      inner: Arc::clone(&self.inner),
    }
  }
}

impl<T> fmt::Debug for ResumeStore<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("ResumeStore")
      .field("config", &self.inner.config)
      .finish_non_exhaustive()
  }
}

/// Why [`ResumeStore::adopt`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
  /// No such live session.
  Unknown,
  /// The session belongs to another principal (counted).
  Principal,
}

/// A connection's hold on its session: dropping it detaches the session.
pub struct Attachment<T: Send + 'static> {
  store: ResumeStore<T>,
  session: Arc<Session<T>>,
}

impl<T: Send + 'static> fmt::Debug for Attachment<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("Attachment").field(&self.session).finish()
  }
}

impl<T: Send + 'static> Attachment<T> {
  /// The attached session.
  pub fn session(&self) -> &Arc<Session<T>> {
    &self.session
  }
}

impl<T: Send + 'static> Drop for Attachment<T> {
  fn drop(&mut self) {
    self.store.detach(&self.session);
  }
}

impl<T: Send + 'static> ResumeStore<T> {
  /// An empty store with `config`'s bounds.
  pub fn new(config: ResumeConfig) -> Self {
    Self {
      inner: Arc::new(Inner {
        config,
        sessions: Mutex::new(Sessions {
          map: HashMap::new(),
          detached: VecDeque::new(),
          detached_count: 0,
        }),
        refused_principal: AtomicU64::new(0),
        expired: AtomicU64::new(0),
      }),
    }
  }

  /// The bounds.
  pub fn config(&self) -> &ResumeConfig {
    &self.inner.config
  }

  /// A fresh session for a new connection of `principal`.
  pub fn open(&self, principal: Option<String>) -> Attachment<T> {
    self.release_expired();
    let session = Arc::new(Session {
      id: SessionId::random(),
      principal,
      config: self.inner.config.clone(),
      state: Mutex::new(State {
        next_seq: 0,
        attached: 1,
        detached_at: None,
        calls: HashMap::new(),
        order: BTreeMap::new(),
        messages: 0,
        bytes: 0,
      }),
    });
    lock(&self.inner.sessions)
      .map
      .insert(session.id, Arc::clone(&session));
    Attachment {
      store: self.clone(),
      session,
    }
  }

  /// Attach session `id` to a connection of `principal`.
  pub(crate) fn adopt(
    &self,
    id: SessionId,
    principal: Option<&str>,
  ) -> Result<Attachment<T>, Refusal> {
    self.release_expired();
    let mut sessions = lock(&self.inner.sessions);
    let session = sessions.map.get(&id).cloned().ok_or(Refusal::Unknown)?;
    if session.principal() != principal {
      drop(sessions);
      self.inner.refused_principal.fetch_add(1, Ordering::Relaxed);
      tracing::warn!(target: TARGET, "resume refused: the session belongs to another principal");
      return Err(Refusal::Principal);
    }
    let mut state = lock(&session.state);
    state.attached += 1;
    if state.detached_at.take().is_some() {
      sessions.detached_count -= 1;
    }
    drop(state);
    drop(sessions);
    Ok(Attachment {
      store: self.clone(),
      session,
    })
  }

  /// Count a resumed call answered `ResumeExpired`.
  pub(crate) fn count_expired(&self) {
    self.inner.expired.fetch_add(1, Ordering::Relaxed);
  }

  fn detach(&self, session: &Arc<Session<T>>) {
    let now = self.inner.config.now();
    let mut released = Vec::new();
    let mut sessions = lock(&self.inner.sessions);
    let mut state = lock(&session.state);
    state.attached = state.attached.saturating_sub(1);
    if state.attached > 0 {
      return;
    }
    if state.calls.is_empty() {
      drop(state);
      sessions.map.remove(&session.id);
      return;
    }
    state.detached_at = Some(now);
    drop(state);
    sessions.detached.push_back((now, session.id));
    sessions.detached_count += 1;
    while sessions.detached_count > self.inner.config.max_detached {
      let Some((at, id)) = sessions.detached.pop_front() else {
        break;
      };
      if let Some(records) = Self::take_detached(&mut sessions, at, id) {
        released.push(records);
      }
    }
    drop(sessions);
    drop(released);
    self.schedule_release();
  }

  /// Remove session `id` if it is still detached since `at`, returning its records to drop.
  fn take_detached(
    sessions: &mut Sessions<T>,
    at: Duration,
    id: SessionId,
  ) -> Option<Vec<Record<T>>> {
    let session = sessions.map.get(&id)?;
    if lock(&session.state).detached_at != Some(at) {
      return None;
    }
    let session = sessions.map.remove(&id)?;
    sessions.detached_count -= 1;
    Some(session.release_all())
  }

  fn schedule_release(&self) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
      return;
    };
    let weak: Weak<Inner<T>> = Arc::downgrade(&self.inner);
    let after = self.inner.config.retention + Duration::from_millis(5);
    runtime.spawn(async move {
      tokio::time::sleep(after).await;
      if let Some(inner) = weak.upgrade() {
        ResumeStore { inner }.release_expired();
      }
    });
  }

  /// Release every session detached for the retention period or longer (their calls are
  /// interrupted); returns how many.
  pub fn release_expired(&self) -> usize {
    let now = self.inner.config.now();
    let retention = self.inner.config.retention;
    let mut released = Vec::new();
    let mut sessions = lock(&self.inner.sessions);
    while let Some(&(at, id)) = sessions.detached.front() {
      if now.saturating_sub(at) < retention {
        break;
      }
      sessions.detached.pop_front();
      if let Some(records) = Self::take_detached(&mut sessions, at, id) {
        released.push(records);
      }
    }
    drop(sessions);
    let count = released.len();
    drop(released);
    count
  }

  /// What the store holds, and what it refused.
  pub fn stats(&self) -> ResumeStats {
    let sessions = lock(&self.inner.sessions);
    let mut stats = ResumeStats {
      sessions: sessions.map.len(),
      detached: sessions.detached_count,
      refused_principal: self.inner.refused_principal.load(Ordering::Relaxed),
      expired: self.inner.expired.load(Ordering::Relaxed),
      ..ResumeStats::default()
    };
    for session in sessions.map.values() {
      let state = lock(&session.state);
      stats.calls += state.calls.len();
      stats.messages += state.messages;
      stats.bytes += state.bytes;
    }
    stats
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::wire::{FrameKind, ResumeCall};
  use serde_json::json;

  fn id(n: u64) -> CallId {
    CallId::new(n).unwrap()
  }

  fn item(call: u64, n: i64) -> Frame {
    Frame::new(id(call), FrameKind::Item).with_payload(json!(n))
  }

  fn store(config: ResumeConfig) -> ResumeStore<()> {
    ResumeStore::new(config)
  }

  fn record(session: &Session<()>, call: u64, n: i64) -> Cursor {
    session
      .record_item(id(call), item(call, n), |f| {
        Ok::<_, ()>((f.cursor.unwrap(), 10))
      })
      .unwrap()
      .unwrap()
  }

  fn values(replay: &Replay) -> Vec<Value> {
    replay
      .frames
      .iter()
      .map(|f| f.payload.clone().unwrap())
      .collect()
  }

  fn resume_of(calls: Value) -> Value {
    json!({"session": SessionId::random().to_string(), "calls": calls})
  }

  #[test]
  fn cursors_and_resume_payloads_decode_bounded() {
    assert_eq!(decode_cursor("c1.AgAAAAAAAAAH"), Ok(Cursor::sequence(7)));
    assert_eq!(
      decode_cursor(&Cursor::events(7).to_string()),
      Err(ResumeError::NotSequence)
    );
    assert_eq!(
      decode_cursor(&"x".repeat(65)),
      Err(ResumeError::Cursor(CursorError::TooLong))
    );
    assert_eq!(
      decode_cursor("c1.AgAAAAAAAAAH="),
      Err(ResumeError::Cursor(CursorError::Malformed))
    );
    let seq = |n| Cursor::sequence(n).to_string();
    let resume = decode_resume(&resume_of(json!([{"id": 3, "cursor": seq(2)}]))).unwrap();
    assert_eq!(
      resume.calls,
      vec![ResumeCall {
        id: id(3),
        cursor: Cursor::sequence(2)
      }]
    );
    let twice = resume_of(json!([{"id": 3, "cursor": seq(2)}, {"id": 3, "cursor": seq(2)}]));
    assert!(matches!(
      decode_resume(&twice),
      Err(ResumeError::Malformed(_))
    ));
    let zero = resume_of(json!([{"id": 0, "cursor": seq(2)}]));
    assert!(matches!(
      decode_resume(&zero),
      Err(ResumeError::Malformed(_))
    ));
    let events = resume_of(json!([{"id": 1, "cursor": Cursor::events(2).to_string()}]));
    assert_eq!(decode_resume(&events), Err(ResumeError::NotSequence));
    let many: Vec<Value> = (1..=MAX_RESUME_CALLS as u64 + 1)
      .map(|n| json!({"id": n, "cursor": seq(0)}))
      .collect();
    assert!(matches!(
      decode_resume(&resume_of(Value::Array(many))),
      Err(ResumeError::Malformed(_))
    ));
    assert!(matches!(
      decode_resume(&json!("nope")),
      Err(ResumeError::Malformed(_))
    ));
    assert!(ResumeError::Expired.to_string().contains("expired"));
  }

  #[test]
  fn config_bounds_and_reports() {
    let config = ResumeConfig::new()
      .max_messages(3)
      .max_bytes(40)
      .retention(Duration::ZERO)
      .max_detached(0)
      .clock(TestClock::new());
    assert_eq!(config.messages_limit(), 3);
    assert_eq!(config.bytes_limit(), 40);
    assert_eq!(config.retention_period(), Duration::from_millis(1));
    assert_eq!(config.detached_limit(), 1);
    assert!(format!("{config:?}").contains("max_messages: 3"));
    let defaults = ResumeConfig::default();
    assert_eq!(
      (defaults.messages_limit(), defaults.bytes_limit()),
      (1024, 1 << 20)
    );
    assert_eq!(defaults.retention_period(), Duration::from_secs(30));
    let clock = SystemClock::default();
    assert!(clock.now() <= clock.now());
  }

  #[test]
  fn replay_follows_the_cursor_and_credits_prune() {
    let store = store(ResumeConfig::new().clock(TestClock::new()));
    let attached = store.open(Some("ada".into()));
    let session = attached.session();
    assert_eq!(session.principal(), Some("ada"));
    session.insert(id(1), ());
    let cursors: Vec<Cursor> = (0..5).map(|n| record(session, 1, n)).collect();
    assert_eq!(cursors[0], Cursor::sequence(1));
    // The client saw two items: three follow, and the two count as credited.
    let replay = session.replay(id(1), cursors[1], || {}).unwrap();
    assert_eq!(values(&replay), vec![json!(2), json!(3), json!(4)]);
    assert_eq!(replay.credited, 2);
    // Credits free the items the client has; a cursor before them expired.
    assert!(session.ack(id(1), 2, || true));
    assert_eq!(store.stats().messages, 1);
    assert_eq!(
      session.replay(id(1), cursors[0], || {}).unwrap_err(),
      ResumeError::Expired
    );
    // A cursor the call never reached, another source, an unknown call.
    assert!(session.replay(id(1), Cursor::sequence(99), || {}).is_err());
    assert!(session.replay(id(1), Cursor::events(4), || {}).is_err());
    assert!(session.replay(id(2), Cursor::sequence(0), || {}).is_err());
    // The exit follows the items.
    assert!(session.record_exit(id(1), Frame::new(id(1), FrameKind::Exit), 5));
    let replay = session.replay(id(1), cursors[4], || {}).unwrap();
    assert_eq!(replay.frames.len(), 1);
    assert_eq!(replay.frames[0].kind, FrameKind::Exit);
    // Released calls stop their producers.
    session.remove(id(1));
    assert!(
      session
        .record_item(id(1), item(1, 0), |_| Ok::<_, ()>(((), 1)))
        .is_none()
    );
    assert!(!session.record_exit(id(1), Frame::new(id(1), FrameKind::Exit), 1));
    assert!(session.ext(id(1)).is_none());
    assert!(!session.ack(id(1), 1, || false));
    assert!(session.ack(id(1), 1, || true));
    // An encoding failure buffers nothing.
    session.insert(id(2), ());
    assert_eq!(
      session.record_item(id(2), item(2, 0), |_| Err::<((), usize), _>("big")),
      Some(Err("big"))
    );
    assert_eq!(session.ext(id(2)), Some(()));
    assert_eq!(store.stats().messages, 0);
    assert!(format!("{session:?} {attached:?} {store:?}").contains("Session"));
  }

  #[test]
  fn buffers_are_bounded_by_messages_bytes_and_age() {
    let clock = TestClock::new();
    let config = ResumeConfig::new()
      .max_messages(4)
      .max_bytes(35)
      .clock(clock.clone());
    let store = store(config);
    let attached = store.open(None);
    let session = attached.session();
    session.insert(id(1), ());
    session.insert(id(2), ());
    for n in 0..3 {
      record(session, 1, n);
    }
    // 10 bytes each: the fourth overruns 35 bytes, the oldest goes.
    record(session, 2, 0);
    let stats = store.stats();
    assert_eq!((stats.messages, stats.bytes), (3, 30));
    assert_eq!(
      session
        .replay(id(1), Cursor::sequence(0), || {})
        .unwrap_err(),
      ResumeError::Expired
    );
    // Exits count too; evicting a finished call's exit forgets the call.
    assert!(session.record_exit(id(2), Frame::new(id(2), FrameKind::Exit), 1));
    for n in 3..7 {
      record(session, 1, n);
    }
    assert!(session.ext(id(2)).is_none());
    assert_eq!((store.stats().messages, store.stats().bytes), (3, 30));
    // Frames older than the retention are evicted.
    clock.advance(Duration::from_secs(31));
    let fresh = record(session, 1, 9);
    assert_eq!(store.stats().messages, 1);
    assert!(session.replay(id(1), fresh, || {}).is_ok());
    // A record replaced by a new call of the same id starts afresh.
    session.insert(id(1), ());
    assert_eq!(store.stats().messages, 0);
  }

  #[test]
  fn sessions_are_bound_to_their_principal_and_released_after_retention() {
    let clock = TestClock::new();
    let store = store(ResumeConfig::new().max_detached(1).clock(clock.clone()));
    let first = store.open(Some("ada".into()));
    let sid = first.session().id();
    first.session().insert(id(1), ());
    drop(first);
    assert_eq!(store.stats().detached, 1);
    assert_eq!(
      store.adopt(sid, Some("eve")).unwrap_err(),
      Refusal::Principal
    );
    assert_eq!(store.adopt(sid, None).unwrap_err(), Refusal::Principal);
    assert_eq!(store.stats().refused_principal, 2);
    assert_eq!(
      store.adopt(SessionId::random(), Some("ada")).unwrap_err(),
      Refusal::Unknown
    );
    // Re-attached, then detached again.
    let again = store.adopt(sid, Some("ada")).unwrap();
    assert_eq!(store.stats().detached, 0);
    drop(again);
    clock.advance(Duration::from_secs(29));
    assert_eq!(store.release_expired(), 0);
    clock.advance(Duration::from_secs(1));
    assert_eq!(store.release_expired(), 1);
    assert_eq!(
      store.stats(),
      ResumeStats {
        refused_principal: 2,
        ..ResumeStats::default()
      }
    );
    // Sessions without calls go when their connection does.
    drop(store.open(None));
    assert_eq!(store.stats().sessions, 0);
    // Beyond the detached cap the oldest is released.
    let a = store.open(None);
    a.session().insert(id(1), ());
    let b = store.open(None);
    b.session().insert(id(1), ());
    let a_id = a.session().id();
    drop(a);
    drop(b);
    let stats = store.stats();
    assert_eq!((stats.sessions, stats.detached), (1, 1));
    assert!(store.adopt(a_id, None).is_err());
    store.count_expired();
    assert_eq!(store.stats().expired, 1);
    assert_eq!(store.config().detached_limit(), 1);
  }

  #[tokio::test]
  async fn released_records_interrupt_their_tasks() {
    let store = store(ResumeConfig::new().retention(Duration::from_millis(1)));
    let attached = store.open(None);
    let session = Arc::clone(attached.session());
    session.insert(id(1), ());
    let task = tokio::spawn(std::future::pending::<()>());
    session.set_abort(id(1), task.abort_handle());
    session.remove(id(1));
    assert!(task.await.unwrap_err().is_cancelled());
    // A task for a call already gone is interrupted at once.
    let task = tokio::spawn(std::future::pending::<()>());
    session.set_abort(id(7), task.abort_handle());
    assert!(task.await.unwrap_err().is_cancelled());
    // A detached session is released by the timer after the retention.
    session.insert(id(2), ());
    let task = tokio::spawn(std::future::pending::<()>());
    session.set_abort(id(2), task.abort_handle());
    drop(attached);
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(store.stats().sessions, 0);
  }
}
