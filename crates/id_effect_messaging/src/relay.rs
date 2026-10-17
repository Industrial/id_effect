//! [`Relay`]: publishes the committed rows of an [`OutboxStore`] through any
//! [`MessageBroker`], at least once, with retries on a core [`Schedule`].

use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use id_effect::{Effect, HNil, Never, Schedule, UtcDateTime, WallClock, sleep, succeed, suspend};

use crate::broker::{BrokerError, MessageBroker};
use crate::outbox::{OutboxError, OutboxStore, Pending, earlier, later};

/// The first delay after a failed store operation in [`Relay::run`] (doubling up to
/// [`MAX_ERROR_BACKOFF`]).
const ERROR_BACKOFF: Duration = Duration::from_millis(100);
const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(30);

/// The default publish retry of a [`RelayConfig`]: exponential from 500 ms, at most 5 min apart,
/// jittered, forever (a message is never given up on; see [`RelayConfig::retry`]).
pub fn default_relay_retry() -> Schedule<(), BrokerError> {
  Schedule::exponential(Duration::from_millis(500))
    // `union` takes the shorter delay: at most five minutes.
    .union(Schedule::spaced(Duration::from_secs(300)))
    .jittered()
    .map(|_| ())
}

/// The delay before the attempt after attempt number `attempt` (from 1) failed with `error`, or
/// `None` when `schedule` is done.
///
/// A [`Schedule`]'s state lives in memory only and a row may be retried by another relay, so the
/// delay is replayed: the schedule is stepped `attempt` times from [`Schedule::start`] with
/// `error` as the input, on a clock that advances by each delay, and the last delay is returned.
pub fn retry_delay(
  schedule: &Schedule<(), BrokerError>,
  error: &BrokerError,
  attempt: u32,
) -> Option<Duration> {
  let mut now = Instant::now();
  let mut state = schedule.start();
  let mut delay = None;
  for _ in 0..attempt.max(1) {
    let (next, (), step) = schedule.step(now, error, state).into_parts();
    let step = step?;
    now = now.checked_add(step).unwrap_or(now);
    state = next;
    delay = Some(step);
  }
  delay
}

/// `host:pid:random`, with the host from `$HOSTNAME` (or `localhost`).
fn generate_id() -> String {
  static NEXT: AtomicU64 = AtomicU64::new(0);
  let mut h = RandomState::new().build_hasher();
  h.write_u64(NEXT.fetch_add(1, Ordering::Relaxed));
  let host = std::env::var("HOSTNAME")
    .ok()
    .filter(|h| !h.is_empty())
    .unwrap_or_else(|| "localhost".to_owned());
  format!(
    "{host}:{}:{:08x}",
    std::process::id(),
    h.finish() & 0xffff_ffff
  )
}

/// A [`Relay`]'s settings.
#[derive(Clone, Debug)]
pub struct RelayConfig {
  /// The relay's id, the owner of its leases (`locked_by`): unique per relay (default
  /// `host:pid:random`).
  pub id: String,
  /// The most rows one pass claims (default 100).
  pub batch: u32,
  /// How long [`Relay::run`] waits after a pass that did not fill its batch (default 1 s, on the
  /// executor clock).
  pub poll_interval: Duration,
  /// How long a claimed row is leased to the relay (default 30 s). A relay that crashes holds
  /// its rows that long; a batch must publish within it, or another relay may publish the rest
  /// again (still at least once).
  pub lease: Duration,
  /// When a failed publish is tried again ([`retry_delay`]; default [`default_relay_retry`]).
  /// Once a finite schedule is done the row is dead: kept, never published.
  pub retry: Schedule<(), BrokerError>,
  /// Delete sent rows this old (default 1 day; `None` keeps them).
  pub prune_after: Option<Duration>,
  /// How often [`Relay::run`] prunes (default 1 min).
  pub prune_every: Duration,
  /// The clock of every row time (bind the one the [`Outbox`](crate::Outbox) reads).
  pub clock: WallClock,
}

impl Default for RelayConfig {
  fn default() -> Self {
    Self {
      id: generate_id(),
      batch: 100,
      poll_interval: Duration::from_secs(1),
      lease: Duration::from_secs(30),
      retry: default_relay_retry(),
      prune_after: Some(Duration::from_secs(24 * 3600)),
      prune_every: Duration::from_secs(60),
      clock: WallClock::system(),
    }
  }
}

/// What a [`Relay`] pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelayReport {
  /// Rows claimed.
  pub claimed: u64,
  /// Rows published and marked sent.
  pub published: u64,
  /// Rows whose publish failed, to be retried.
  pub retrying: u64,
  /// Rows whose publish failed with the retry schedule done.
  pub dead: u64,
  /// Rows published whose lease had passed to another relay (that relay publishes them again).
  pub lost: u64,
  /// Rows given back unattempted after a failed publish.
  pub released: u64,
}

impl RelayReport {
  /// Whether a publish failed.
  pub fn failed(&self) -> bool {
    self.retrying + self.dead > 0
  }

  fn add(mut self, other: RelayReport) -> Self {
    self.claimed += other.claimed;
    self.published += other.published;
    self.retrying += other.retrying;
    self.dead += other.dead;
    self.lost += other.lost;
    self.released += other.released;
    self
  }
}

struct Inner {
  store: Arc<dyn OutboxStore>,
  broker: Arc<dyn MessageBroker>,
  config: RelayConfig,
}

/// Publishes the outbox: claims committed rows from an [`OutboxStore`], publishes each through a
/// [`MessageBroker`] with its staged [`MessageId`](crate::MessageId), and marks it sent.
///
/// - **At least once.** A row is marked sent after the broker accepted it. A relay that crashes
///   in between (or loses its lease) leaves the row to be claimed again once the lease expires,
///   so the message may be published twice; consumers deduplicate on the message id with the
///   [`Inbox`](crate::Inbox).
/// - **Many relays.** The claim leases rows with `SKIP LOCKED`, so relays on several nodes share
///   the outbox and never publish the same row at the same time.
/// - **Retries.** A failed publish ([`BrokerError`]) makes the row claimable again after the
///   [`RelayConfig::retry`] delay for its attempt number, and gives back the rest of the batch
///   (a broker outage fails them all). Rows are claimed oldest first, but a retried row is
///   published after younger ones: there is no ordering guarantee across a failure.
/// - **Pruning.** [`run`](Self::run) deletes the sent rows older than
///   [`RelayConfig::prune_after`].
///
/// Clones share the store, broker and settings.
#[derive(Clone)]
pub struct Relay {
  inner: Arc<Inner>,
}

impl fmt::Debug for Relay {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Relay")
      .field("config", &self.inner.config)
      .finish_non_exhaustive()
  }
}

impl Relay {
  /// A relay from `store` to `broker` with the default [`RelayConfig`].
  pub fn new(store: Arc<dyn OutboxStore>, broker: Arc<dyn MessageBroker>) -> Self {
    Self {
      inner: Arc::new(Inner {
        store,
        broker,
        config: RelayConfig::default(),
      }),
    }
  }

  /// With `config` instead.
  #[must_use]
  pub fn with_config(self, config: RelayConfig) -> Self {
    Self {
      inner: Arc::new(Inner {
        store: Arc::clone(&self.inner.store),
        broker: Arc::clone(&self.inner.broker),
        config,
      }),
    }
  }

  /// The settings.
  pub fn config(&self) -> &RelayConfig {
    &self.inner.config
  }

  /// One pass: claims up to [`batch`](RelayConfig::batch) rows and publishes them in order,
  /// stopping at the first failed publish (that row is retried later, the rest given back).
  /// Store errors fail it.
  pub fn run_once(&self) -> Effect<RelayReport, OutboxError> {
    let inner = Arc::clone(&self.inner);
    suspend(move || {
      let c = &inner.config;
      let claim = c.clock.now().flat_map({
        let inner = Arc::clone(&inner);
        move |now| {
          let c = &inner.config;
          inner.store.claim(&c.id, c.batch, now, c.lease)
        }
      });
      claim.flat_map(move |rows| {
        let report = RelayReport {
          claimed: rows.len() as u64,
          ..RelayReport::default()
        };
        publish_rows(inner, rows.into_iter(), report)
      })
    })
  }

  /// Passes until one claims nothing or a publish fails, then returns what they did together:
  /// for tests and batch programs. Store errors fail it.
  pub fn run_until_idle(&self) -> Effect<RelayReport, OutboxError> {
    idle_pass(self.clone(), RelayReport::default())
  }

  /// Runs until interrupted: passes back to back while they fill their batch, else every
  /// [`poll_interval`](RelayConfig::poll_interval); prunes every
  /// [`prune_every`](RelayConfig::prune_every). Store errors are retried with backoff (100 ms
  /// doubling to 30 s) and never end it. Interrupting it gives back the rows it holds.
  pub fn run(&self) -> Effect<(), Never> {
    let relay = self.clone();
    let store = Arc::clone(&self.inner.store);
    let id = self.inner.config.id.clone();
    let release = suspend(move || store.release(&id).map(|_| ()).catch(|_| succeed(())));
    suspend(move || poll(relay, None, ERROR_BACKOFF)).ensuring(release)
  }
}

/// Publishes `rows` in order, settling each.
fn publish_rows(
  inner: Arc<Inner>,
  mut rows: std::vec::IntoIter<Pending>,
  report: RelayReport,
) -> Effect<RelayReport, OutboxError> {
  let Some(row) = rows.next() else {
    return succeed(report);
  };
  let published = inner
    .broker
    .publish(&row.topic, row.message.clone())
    .map(|()| None)
    .catch(|e| succeed(Some(e)));
  published.flat_map(move |failure| {
    let now = inner.config.clock.now::<OutboxError, HNil>();
    match failure {
      None => now.flat_map(move |now| {
        let c = &inner.config;
        inner
          .store
          .mark_sent(row.id, &c.id, now)
          .flat_map(move |applied| {
            let mut report = report;
            if applied {
              report.published += 1;
            } else {
              report.lost += 1;
            }
            publish_rows(inner, rows, report)
          })
      }),
      Some(error) => now.flat_map(move |now| {
        let c = &inner.config;
        let text = error.to_string();
        let settle = match retry_delay(&c.retry, &error, row.attempt) {
          Some(delay) => inner
            .store
            .retry_at(row.id, &c.id, later(now, delay), &text)
            .map(|_| (1, 0)),
          None => inner.store.dead(row.id, &c.id, now, &text).map(|_| (0, 1)),
        };
        let release = inner.store.release(&c.id);
        settle.flat_map(move |(retrying, dead)| {
          release.map(move |released| RelayReport {
            retrying: report.retrying + retrying,
            dead: report.dead + dead,
            released: report.released + released,
            ..report
          })
        })
      }),
    }
  })
}

fn idle_pass(relay: Relay, total: RelayReport) -> Effect<RelayReport, OutboxError> {
  relay.run_once().flat_map(move |report| {
    let total = total.add(report);
    if report.claimed == 0 || report.failed() {
      succeed(total)
    } else {
      idle_pass(relay, total)
    }
  })
}

/// Prunes when `next_prune` is due; the next due time.
fn prune_due(
  relay: &Relay,
  now: UtcDateTime,
  next_prune: Option<UtcDateTime>,
) -> Effect<Option<UtcDateTime>, Never> {
  let c = &relay.inner.config;
  let Some(keep) = c.prune_after else {
    return succeed(None);
  };
  if next_prune.is_some_and(|at| now < at) {
    return succeed(next_prune);
  }
  let next = Some(later(now, c.prune_every));
  relay
    .inner
    .store
    .prune(earlier(now, keep))
    .map(move |_| next)
    .catch(move |_| succeed(next))
}

/// One [`Relay::run`] step: prune when due, a pass, then the wait the pass calls for.
fn poll(relay: Relay, next_prune: Option<UtcDateTime>, backoff: Duration) -> Effect<(), Never> {
  let clock = relay.inner.config.clock.clone();
  clock.now::<Never, HNil>().flat_map(move |now| {
    prune_due(&relay, now, next_prune).flat_map(move |next_prune| {
      relay
        .run_once()
        .map(Some)
        .catch(|_| succeed(None))
        .flat_map(move |pass| {
          let c = &relay.inner.config;
          let (wait, backoff) = match pass {
            Some(r) if r.claimed >= u64::from(c.batch) && !r.failed() => {
              (Duration::ZERO, ERROR_BACKOFF)
            }
            Some(_) => (c.poll_interval, ERROR_BACKOFF),
            None => (backoff, (backoff * 2).min(MAX_ERROR_BACKOFF)),
          };
          let next = suspend(move || poll(relay, next_prune, backoff));
          if wait.is_zero() {
            next
          } else {
            sleep(wait).flat_map(move |()| next)
          }
        })
    })
  })
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit, UtcDateTime};

  use super::*;
  use crate::outbox::MemoryOutbox;
  use crate::{MemoryBroker, Message, Topic};

  fn run<A: Send + 'static>(eff: Effect<A, OutboxError>) -> A {
    match eff.block_on() {
      Exit::Success(a) => a,
      Exit::Failure(Cause::Fail(e)) => panic!("failed: {e}"),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn t(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn setup(retry: Schedule<(), BrokerError>) -> (Relay, MemoryOutbox, MemoryBroker, WallClock) {
    let clock = WallClock::anchored(Instant::now(), t(1000));
    let outbox = MemoryOutbox::new().with_clock(clock.clone());
    let broker = MemoryBroker::new();
    let config = RelayConfig {
      id: "relay-a".into(),
      batch: 2,
      retry,
      clock: clock.clone(),
      ..RelayConfig::default()
    };
    let relay = Relay::new(Arc::new(outbox.clone()), Arc::new(broker.clone())).with_config(config);
    (relay, outbox, broker, clock)
  }

  #[test]
  fn retry_delay_replays_the_schedule_per_attempt() {
    let exp: Schedule<(), BrokerError> = Schedule::exponential(Duration::from_secs(1)).map(|_| ());
    let e = BrokerError::Connection("down".into());
    let delays: Vec<_> = (1..=4).map(|n| retry_delay(&exp, &e, n)).collect();
    assert_eq!(delays, [1, 2, 4, 8].map(|s| Some(Duration::from_secs(s))));
    let twice: Schedule<(), BrokerError> = Schedule::recurs(2).map(|_| ());
    assert_eq!(retry_delay(&twice, &e, 2), Some(Duration::ZERO));
    assert_eq!(retry_delay(&twice, &e, 3), None);
    let d = retry_delay(&default_relay_retry(), &e, 40).unwrap();
    assert!(
      d <= Duration::from_secs(360),
      "capped at 5 min (+20 % jitter): {d:?}"
    );
  }

  #[test]
  fn relay_publishes_in_order_with_the_staged_ids() {
    let (relay, outbox, broker, _) = setup(default_relay_retry());
    let orders = Topic::new("orders").unwrap();
    for n in 0..5 {
      let m = Message::new(format!("o-{n}")).with_id(format!("id-{n}"));
      run(outbox.stage(&orders, m));
    }
    let report = run(relay.run_until_idle());
    assert_eq!((report.claimed, report.published), (5, 5));
    let ids: Vec<String> = broker
      .messages(&orders)
      .iter()
      .map(|m| m.id().to_string())
      .collect();
    assert_eq!(ids, ["id-0", "id-1", "id-2", "id-3", "id-4"]);
    assert_eq!((outbox.pending(), outbox.sent()), (0, 5));
    assert_eq!(
      run(relay.run_once()),
      RelayReport::default(),
      "nothing left"
    );
  }

  #[test]
  fn a_broker_outage_retries_on_the_schedule() {
    let exp: Schedule<(), BrokerError> = Schedule::exponential(Duration::from_secs(10)).map(|_| ());
    let (relay, outbox, broker, clock) = setup(exp);
    let orders = Topic::new("orders").unwrap();
    let first = run(outbox.stage(&orders, Message::new("a")));
    run(outbox.stage(&orders, Message::new("b")));
    broker.fail_with(Some("down"));
    let report = run(relay.run_until_idle());
    assert_eq!(
      (report.claimed, report.retrying, report.released),
      (2, 1, 1),
      "the failed row waits, the rest is given back"
    );
    assert_eq!(
      outbox.last_error(first).as_deref(),
      Some("broker connection: down")
    );
    broker.fail_with(None);
    // The second row was released unattempted: published now; the first waits out its 10 s.
    assert_eq!(run(relay.run_until_idle()).published, 1);
    assert_eq!(broker.messages(&orders)[0].payload(), b"b");
    clock.jump(Duration::from_secs(10));
    assert_eq!(run(relay.run_until_idle()).published, 1);
    assert_eq!((outbox.pending(), outbox.sent()), (0, 2));
  }

  #[test]
  fn a_done_schedule_leaves_the_row_dead() {
    let once: Schedule<(), BrokerError> = Schedule::recurs(1).map(|_| ());
    let (relay, outbox, broker, _) = setup(once);
    let orders = Topic::new("orders").unwrap();
    let id = run(outbox.stage(&orders, Message::new("a")));
    broker.fail_with(Some("rejected"));
    assert_eq!(run(relay.run_until_idle()).retrying, 1);
    let report = run(relay.run_until_idle());
    assert_eq!(report.dead, 1);
    assert!(report.failed());
    assert_eq!(
      outbox.dead_rows(),
      [(id, "broker connection: rejected".to_owned())]
    );
    broker.fail_with(None);
    assert_eq!(
      run(relay.run_until_idle()),
      RelayReport::default(),
      "never again"
    );
  }

  #[test]
  fn run_publishes_prunes_and_releases_on_interrupt() {
    let (relay, outbox, broker, clock) = setup(default_relay_retry());
    let config = relay.config().clone();
    let relay = relay.with_config(RelayConfig {
      poll_interval: Duration::from_millis(5),
      prune_after: Some(Duration::ZERO),
      prune_every: Duration::ZERO,
      ..config
    });
    let orders = Topic::new("orders").unwrap();
    for n in 0..3 {
      run(outbox.stage(&orders, Message::new(format!("{n}"))));
    }
    let fiber = std::thread::spawn({
      let relay = relay.clone();
      move || relay.run().timeout(Duration::from_millis(300)).block_on()
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while broker.messages(&orders).len() < 3 && Instant::now() < deadline {
      std::thread::sleep(Duration::from_millis(5));
    }
    clock.jump(Duration::from_secs(1));
    assert!(matches!(fiber.join().unwrap(), Exit::Success(None)));
    assert_eq!(broker.messages(&orders).len(), 3);
    assert_eq!(
      (outbox.pending(), outbox.sent()),
      (0, 0),
      "sent rows pruned"
    );
    assert!(format!("{relay:?}").contains("relay-a"));
  }
}
