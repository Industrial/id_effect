//! Subscriptions backed by an `id_effect_events` log (r12-e12-2-subscriptions, design note §6):
//! the cursor is the global position, a call replays from it and then tails without a gap, a
//! resume neither skips nor repeats, interrupting the call releases the reader, a slow
//! subscriber is lagged rather than buffered, and every resume runs the guards again.
//!
//! The first three run against the in-memory store and again against PostgreSQL in a container
//! this test binary starts itself (never the ambient `DATABASE_URL`). Without Docker the
//! PostgreSQL tests print `SKIP` and pass, unless `ID_EFFECT_REQUIRE_PG=1` (or
//! `ID_EFFECT_REQUIRE_DOCKER=1`), where they fail.

#![cfg(feature = "events")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use id_effect::{Cause, DynEnv, Effect, Exit, HNil, fail, schema, succeed};
use id_effect_axum::route::GuardFuture;
use id_effect_events::{
  EventLog, EventStore, EventStoreError, MemoryEventStore, RecordedEvent, SqlEventJournal,
};
use id_effect_rpc::cancel::CallControl;
use id_effect_rpc::handler::CallContext;
use id_effect_rpc::kinds::CallFrames;
use id_effect_rpc::registry::RouteGuard;
use id_effect_rpc::server::dispatch::{self, Rejection};
use id_effect_rpc::subscription::TailConfig;
use id_effect_rpc::subscription::events::EventSubscription;
use id_effect_rpc::wire::{CallId, Cursor, FrameKind};
use id_effect_rpc::{Procedure, RpcGroup, WireSchema};
use id_effect_test::{ContainerError, Postgres, Shared, docker_available};
use id_effect_tokio::TokioRuntime;
use serde_json::{Value, json};

const TAG: &str = "Feed.follow";
const POLL: Duration = Duration::from_millis(20);
const WAIT: Duration = Duration::from_secs(15);

// -------------------------------------------------------------------------------------------
// Fixture
// -------------------------------------------------------------------------------------------

/// Runs `eff` on the test's tokio runtime (sqlx needs one): the typed outcome as a `Result`.
async fn run<A: Send + 'static, E: Send + std::fmt::Debug + 'static>(
  eff: Effect<A, E>,
) -> Result<A, E> {
  match TokioRuntime::current().expect("tokio").run(eff).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

async fn append<S: EventStore<i64>>(store: &S, stream: &str, values: &[i64]) {
  run(store.append(stream, values)).await.expect("append");
}

fn unique(name: &str) -> String {
  format!("{name}-{}", uuid::Uuid::new_v4())
}

/// A log that counts its reads.
struct Counting<L> {
  inner: L,
  reads: Arc<AtomicUsize>,
}

impl<L: EventLog<i64>> EventLog<i64> for Counting<L> {
  fn read_all(&self, after: u64, limit: usize) -> Effect<Vec<RecordedEvent<i64>>, EventStoreError> {
    self.reads.fetch_add(1, Ordering::SeqCst);
    self.inner.read_all(after, limit)
  }
}

/// A log whose every read fails.
struct Broken;

impl EventLog<i64> for Broken {
  fn read_all(&self, _: u64, _: usize) -> Effect<Vec<RecordedEvent<i64>>, EventStoreError> {
    fail(EventStoreError::Io("disk on fire at 10.0.0.7".into()))
  }
}

/// Admits requests carrying `x-member: yes`.
struct Member;

impl RouteGuard for Member {
  fn policy(&self) -> &str {
    "member"
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    let member = parts.headers.get("x-member").is_some_and(|v| v == "yes");
    Box::pin(async move {
      if member {
        Ok(())
      } else {
        Err(StatusCode::FORBIDDEN.into_response())
      }
    })
  }
}

/// `Feed.follow`: the amounts of every stream whose id starts with the input prefix.
struct Feed {
  group: RpcGroup,
  source: EventSubscription<i64>,
  reads: Arc<AtomicUsize>,
}

fn feed(log: impl EventLog<i64> + 'static, guarded: bool) -> Feed {
  let reads = Arc::new(AtomicUsize::new(0));
  let source = EventSubscription::new(Counting {
    inner: log,
    reads: Arc::clone(&reads),
  })
  .with_config(
    TailConfig::new()
      .with_batch_size(4)
      .with_poll_interval(POLL),
  );
  let follow = source.clone();
  let handler = move |prefix: String, after: Option<Cursor>, _: &mut DynEnv| {
    let project =
      move |e: RecordedEvent<i64>| e.stream_id.starts_with(&prefix).then_some(e.event.payload);
    match follow.subscribe(after, project) {
      Ok(subscription) => succeed(subscription),
      Err(mismatch) => fail(mismatch),
    }
  };
  let input = WireSchema::new(schema::string::<()>());
  let item = WireSchema::new(schema::i64::<()>());
  let mut group = RpcGroup::new();
  if guarded {
    group.subscription(
      Procedure::subscription(TAG).guard(Member),
      input,
      item,
      handler,
    );
  } else {
    group.subscription(Procedure::subscription(TAG).public(), input, item, handler);
  }
  Feed {
    group,
    source,
    reads,
  }
}

impl Feed {
  /// Subscribe to `prefix` after `after`, through `dispatch::open` (guards) like a transport.
  async fn call(
    &self,
    headers: HeaderMap,
    prefix: &str,
    after: Option<Cursor>,
  ) -> Result<CallFrames, Rejection> {
    let mut parts = http::Request::new(()).into_parts().0;
    parts.headers = headers;
    let entry = dispatch::open(&self.group, &parts, TAG, None).await?;
    dispatch::start_subscription(
      entry,
      CallId::new(1).expect("id"),
      json!(prefix),
      after,
      CallContext::new(DynEnv::new(), "test-call"),
      CallControl::none(),
      2,
    )
  }

  async fn subscribe(&self, prefix: &str, after: Option<Cursor>) -> CallFrames {
    self
      .call(HeaderMap::new(), prefix, after)
      .await
      .expect("subscribe")
  }
}

fn member() -> HeaderMap {
  let mut headers = HeaderMap::new();
  headers.insert("x-member", "yes".parse().expect("header"));
  headers
}

/// The next `n` items (value, cursor); panics on an `exit` frame or after [`WAIT`].
async fn take(frames: &mut CallFrames, n: usize) -> Vec<(i64, Cursor)> {
  let mut items = Vec::with_capacity(n);
  while items.len() < n {
    let frame = tokio::time::timeout(WAIT, frames.next())
      .await
      .expect("an item in time")
      .expect("the call is still open");
    assert_eq!(frame.kind, FrameKind::Item, "{frame:?}");
    let value = frame.payload.as_ref().and_then(Value::as_i64).expect("i64");
    items.push((value, frame.cursor.expect("every item has a cursor")));
  }
  items
}

/// The call's `exit` frame payload.
async fn exit(frames: &mut CallFrames) -> Value {
  let frame = tokio::time::timeout(WAIT, frames.next())
    .await
    .expect("an exit in time")
    .expect("a frame");
  assert_eq!(frame.kind, FrameKind::Exit, "{frame:?}");
  frame.payload.expect("payload")
}

fn values(items: &[(i64, Cursor)]) -> Vec<i64> {
  items.iter().map(|(v, _)| *v).collect()
}

fn increasing(items: &[(i64, Cursor)]) -> bool {
  items.windows(2).all(|w| w[0].1.position < w[1].1.position)
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
  let until = Instant::now() + WAIT;
  while !check() {
    assert!(Instant::now() < until, "timed out waiting for {what}");
    tokio::time::sleep(Duration::from_millis(5)).await;
  }
}

// -------------------------------------------------------------------------------------------
// The three properties, against any store
// -------------------------------------------------------------------------------------------

async fn replays_from_cursor_then_tails<S>(store: S)
where
  S: EventStore<i64> + EventLog<i64> + Clone + 'static,
{
  let prefix = unique("acct");
  let other = unique("other");
  append(&store, &format!("{prefix}-a"), &[1, 2]).await;
  append(&store, &other, &[100]).await;
  append(&store, &format!("{prefix}-b"), &[3]).await;
  let feed = feed(store.clone(), false);

  // From the start: the projection keeps only the prefix's streams.
  let mut first = feed.subscribe(&prefix, None).await;
  let history = take(&mut first, 3).await;
  assert_eq!(values(&history), [1, 2, 3]);
  assert!(increasing(&history));
  drop(first);

  // From the first item's cursor: the rest of the history, then the live tail.
  let mut resumed = feed.subscribe(&prefix, Some(history[0].1)).await;
  assert_eq!(take(&mut resumed, 2).await, history[1..]);
  append(&store, &other, &[200]).await;
  append(&store, &format!("{prefix}-a"), &[4, 5]).await;
  let live = take(&mut resumed, 2).await;
  assert_eq!(values(&live), [4, 5]);
  assert!(live[0].1.position > history[2].1.position);
  assert!(increasing(&live));
}

async fn resume_has_no_gap_or_duplicate<S>(store: S)
where
  S: EventStore<i64> + EventLog<i64> + Clone + Send + Sync + 'static,
{
  const TOTAL: i64 = 40;
  let prefix = unique("tail");
  let noise = unique("noise");
  let feed = feed(store.clone(), false);
  let writer = {
    let (store, prefix) = (store.clone(), prefix.clone());
    tokio::spawn(async move {
      for n in 0..TOTAL {
        append(&store, &prefix, &[n]).await;
        if n % 3 == 0 {
          append(&store, &noise, &[-1]).await;
        }
        tokio::time::sleep(Duration::from_millis(3)).await;
      }
    })
  };

  // Read a few items, disconnect, resume from the last cursor; over and over.
  let mut seen: Vec<(i64, Cursor)> = Vec::new();
  while seen.len() < TOTAL as usize {
    let after = seen.last().map(|(_, c)| *c);
    let mut call = feed.subscribe(&prefix, after).await;
    let n = 7.min(TOTAL as usize - seen.len());
    seen.extend(take(&mut call, n).await);
    drop(call);
  }
  writer.await.expect("writer");
  assert_eq!(values(&seen), (0..TOTAL).collect::<Vec<_>>());
  assert!(increasing(&seen), "no duplicate, no rewind");
  eventually("every reader released", || feed.source.open_readers() == 0).await;
}

async fn interrupt_releases_reader<S>(store: S)
where
  S: EventStore<i64> + EventLog<i64> + Clone + 'static,
{
  let prefix = unique("idle");
  append(&store, &prefix, &[1]).await;
  let feed = feed(store.clone(), false);
  let mut call = feed.subscribe(&prefix, None).await;
  assert_eq!(values(&take(&mut call, 1).await), [1]);
  assert_eq!(feed.source.open_readers(), 1);

  // The reader is tailing the log: it keeps polling while the call is alive.
  let before = feed.reads.load(Ordering::SeqCst);
  let polling = async {
    loop {
      if feed.reads.load(Ordering::SeqCst) >= before + 2 {
        break;
      }
      tokio::time::sleep(Duration::from_millis(5)).await;
    }
  };
  tokio::select! {
    () = polling => {}
    frame = call.next() => panic!("no item expected, got {frame:?}"),
    () = tokio::time::sleep(WAIT) => panic!("the reader is not polling"),
  }

  // Dropping the call interrupts the reader; nothing polls afterwards.
  drop(call);
  eventually("the reader released", || feed.source.open_readers() == 0).await;
  let after = feed.reads.load(Ordering::SeqCst);
  tokio::time::sleep(POLL * 10).await;
  assert_eq!(
    feed.reads.load(Ordering::SeqCst),
    after,
    "no leaked polling fiber"
  );
}

// -------------------------------------------------------------------------------------------
// In-memory store
// -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscription_replays_from_cursor_then_tails() {
  replays_from_cursor_then_tails(MemoryEventStore::<i64>::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscription_resume_has_no_gap_or_duplicate() {
  resume_has_no_gap_or_duplicate(MemoryEventStore::<i64>::new()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscription_interrupt_releases_reader() {
  interrupt_releases_reader(MemoryEventStore::<i64>::new()).await;
}

/// A subscriber that stops reading holds the window and one batch, not the backlog: the reader
/// stops reading the log until it is pulled again, then catches up from its own position.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_subscriber_is_lagged_not_buffered() {
  let store = MemoryEventStore::<i64>::new();
  let prefix = unique("backlog");
  let backlog: Vec<i64> = (0..200).collect();
  append(&store, &prefix, &backlog).await;
  let feed = feed(store, false);
  let mut call = feed.subscribe(&prefix, None).await;
  assert_eq!(values(&take(&mut call, 1).await), [0]);

  // The subscriber stalls: the reader stops after filling the window (2) and one batch (4).
  tokio::time::sleep(POLL * 10).await;
  let stalled = feed.reads.load(Ordering::SeqCst);
  assert!(stalled <= 3, "{stalled} reads while stalled");
  tokio::time::sleep(POLL * 10).await;
  assert_eq!(feed.reads.load(Ordering::SeqCst), stalled);

  // Pulled again, it delivers the whole backlog in order.
  let rest = take(&mut call, 199).await;
  assert_eq!(values(&rest), (1..200).collect::<Vec<_>>());
  assert!(increasing(&rest));
  assert!(feed.reads.load(Ordering::SeqCst) <= 200 / 4 + 3);
}

/// A cursor is not a capability: a resume is a new call, and the guards run again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscription_resume_rechecks_authorization() {
  let store = MemoryEventStore::<i64>::new();
  let prefix = unique("members");
  append(&store, &prefix, &[1, 2, 3]).await;
  let feed = feed(store, true);

  let refused = feed.call(HeaderMap::new(), &prefix, None).await;
  assert!(matches!(refused, Err(Rejection::Refused(ref r)) if r.status() == StatusCode::FORBIDDEN));

  let mut call = feed.call(member(), &prefix, None).await.expect("member");
  let first = take(&mut call, 1).await;
  drop(call);

  // The cursor alone does not resume.
  let replayed = feed.call(HeaderMap::new(), &prefix, Some(first[0].1)).await;
  assert!(
    matches!(replayed, Err(Rejection::Refused(ref r)) if r.status() == StatusCode::FORBIDDEN)
  );

  let mut call = feed
    .call(member(), &prefix, Some(first[0].1))
    .await
    .expect("member resume");
  assert_eq!(values(&take(&mut call, 2).await), [2, 3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cursor_of_another_source_is_a_typed_failure() {
  let feed = feed(MemoryEventStore::<i64>::new(), false);
  let mut call = feed.subscribe("x", Some(Cursor::sequence(3))).await;
  assert_eq!(
    exit(&mut call).await,
    json!({"_tag": "Failure", "error": {"_tag": "CursorMismatch"}})
  );
  assert_eq!(feed.reads.load(Ordering::SeqCst), 0, "the log is not read");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_read_is_an_internal_error_without_its_message() {
  let feed = feed(Broken, false);
  let mut call = feed.subscribe("x", None).await;
  let exit = exit(&mut call).await;
  assert_eq!(
    exit,
    json!({"_tag": "Failure", "error": {"_tag": "InternalError", "id": "test-call"}})
  );
  assert!(!exit.to_string().contains("10.0.0.7"));
}

// -------------------------------------------------------------------------------------------
// PostgreSQL (own container)
// -------------------------------------------------------------------------------------------

static PG: Shared<Postgres> = Shared::new(Postgres::new);

fn pg_required() -> bool {
  ["ID_EFFECT_REQUIRE_PG", "ID_EFFECT_REQUIRE_DOCKER"]
    .iter()
    .any(|var| std::env::var(var).is_ok_and(|v| v == "1"))
}

/// A journal on this binary's own PostgreSQL container, or `None` (SKIP) without Docker.
async fn pg_journal(test: &str) -> Option<SqlEventJournal<i64>> {
  if let Err(e) = tokio::task::spawn_blocking(docker_available)
    .await
    .expect("probe")
  {
    assert!(
      !pg_required(),
      "{test}: ID_EFFECT_REQUIRE_PG=1 but Docker is unavailable: {e}"
    );
    eprintln!("SKIP {test}: {e} (set ID_EFFECT_REQUIRE_PG=1 to require PostgreSQL)");
    return None;
  }
  let pg = run(PG.get::<ContainerError, HNil>())
    .await
    .expect("start PostgreSQL");
  let env = run(pg.sql_layer::<EventStoreError>().build())
    .await
    .expect("connect");
  let journal = SqlEventJournal::from_service(&env.0);
  run(journal.create_table()).await.expect("journal table");
  Some(journal)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_subscription_replays_from_cursor_then_tails() {
  if let Some(journal) = pg_journal("pg_subscription_replays_from_cursor_then_tails").await {
    replays_from_cursor_then_tails(journal).await;
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_subscription_resume_has_no_gap_or_duplicate() {
  if let Some(journal) = pg_journal("pg_subscription_resume_has_no_gap_or_duplicate").await {
    resume_has_no_gap_or_duplicate(journal).await;
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pg_subscription_interrupt_releases_reader() {
  if let Some(journal) = pg_journal("pg_subscription_interrupt_releases_reader").await {
    interrupt_releases_reader(journal).await;
  }
}
