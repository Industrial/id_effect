//! CP [`ClusterSingleton`] — at most one holder cluster-wide.
//!
//! Netsplit story (ADR 0009): unavailable without quorum. Production path
//! uses Postgres advisory-lock leases (`pg` feature via `id_effect_sql` (`pg` feature)).
//! This module ships an **in-memory lease** suitable for single-process
//! tests and as the interface the PG backend implements.
//!
//! Every lease operation is a `next` effect (`Send`, interruptible). The Postgres lease keeps
//! its dedicated session on a daemon fiber (not a thread with its own tokio runtime), so it
//! must be connected from a fiber on a `TokioExecutor` whose runtime has the IO driver enabled
//! (sqlx needs it; `Runtime::new()` / `Runtime::global()` do).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use id_effect::Never;
use id_effect::next::{Effect, fail, succeed, suspend, sync};
#[cfg(feature = "pg")]
use id_effect::{Cause, Exit};
use id_effect_process::Pid;

/// Derive `(key1, key2)` for `pg_try_advisory_lock(key1, key2)` from a singleton name.
///
/// Production `SingletonLease` backends run:
/// `SELECT pg_try_advisory_lock($1, $2)` / `pg_advisory_unlock` with these keys
/// against `id_effect_sql` (`pg` feature) pools (ADR 0009 CP path).
///
/// The keys are the first 8 bytes of `SHA-256(name)`, so every node derives
/// the same pair regardless of Rust version or build (std's `DefaultHasher`
/// gives no such guarantee, and a mismatch would let two nodes hold the
/// "same" singleton).
pub fn advisory_lock_keys(name: &str) -> (i32, i32) {
  use sha2::{Digest, Sha256};
  let digest = Sha256::digest(name.as_bytes());
  let k1 = i32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
  let k2 = i32::from_be_bytes([digest[4], digest[5], digest[6], digest[7]]);
  (k1, k2)
}

/// Why a singleton acquire failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SingletonError {
  /// Another holder currently owns the lease.
  HeldBy(String),
  /// Lease storage unavailable (no PG quorum / pool down).
  Unavailable(String),
}

impl std::fmt::Display for SingletonError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      SingletonError::HeldBy(id) => write!(f, "held by {id}"),
      SingletonError::Unavailable(e) => write!(f, "unavailable: {e}"),
    }
  }
}

impl std::error::Error for SingletonError {}

/// Abstract lease backend. Each operation is an effect run on the caller's fiber.
pub trait SingletonLease: Send + Sync {
  /// Try to acquire `name` for `holder` until `ttl` elapses.
  fn try_acquire(&self, name: &str, holder: &str, ttl: Duration) -> Effect<(), SingletonError>;

  /// Renew an existing lease (must already be held by `holder`).
  fn renew(&self, name: &str, holder: &str, ttl: Duration) -> Effect<(), SingletonError>;

  /// Release if (and only if) `holder` owns it.
  fn release(&self, name: &str, holder: &str) -> Effect<(), SingletonError>;

  /// Current holder id, if any and not expired.
  fn holder(&self, name: &str) -> Effect<Option<String>, Never>;
}

/// A ready effect from a computed result.
fn from_result<A: Send + 'static, E: Send + 'static>(r: Result<A, E>) -> Effect<A, E> {
  match r {
    Ok(a) => succeed(a),
    Err(e) => fail(e),
  }
}

struct MemEntry {
  holder: String,
  until: Instant,
}

/// **Test-only** in-process lease table (single process, TTL-based).
///
/// Provides `SingletonLease` semantics without a database for unit tests and single-node demos.
/// It cannot coordinate across processes — use `PgAdvisoryLease` (feature `pg`) for the CP
/// production path.
#[derive(Clone, Default)]
pub struct MemoryLease {
  inner: Arc<Mutex<HashMap<String, MemEntry>>>,
}

impl MemoryLease {
  /// Empty table.
  pub fn new() -> Self {
    Self::default()
  }

  fn sweep(map: &mut HashMap<String, MemEntry>) {
    let now = Instant::now();
    map.retain(|_, e| e.until > now);
  }

  /// Run `f` on the swept table when the effect runs.
  fn with_table<A: Send + 'static, E: Send + 'static>(
    &self,
    f: impl FnOnce(&mut HashMap<String, MemEntry>) -> Result<A, E> + Send + 'static,
  ) -> Effect<A, E> {
    let inner = Arc::clone(&self.inner);
    suspend(move || {
      let mut map = inner.lock().expect("lease");
      from_result(f(&mut map))
    })
  }
}

impl SingletonLease for MemoryLease {
  fn try_acquire(&self, name: &str, holder: &str, ttl: Duration) -> Effect<(), SingletonError> {
    let (name, holder) = (name.to_string(), holder.to_string());
    self.with_table(move |map| {
      Self::sweep(map);
      match map.get(&name) {
        Some(e) if e.holder != holder => Err(SingletonError::HeldBy(e.holder.clone())),
        _ => {
          map.insert(
            name,
            MemEntry {
              holder,
              until: Instant::now() + ttl,
            },
          );
          Ok(())
        }
      }
    })
  }

  fn renew(&self, name: &str, holder: &str, ttl: Duration) -> Effect<(), SingletonError> {
    let (name, holder) = (name.to_string(), holder.to_string());
    self.with_table(move |map| {
      Self::sweep(map);
      match map.get_mut(&name) {
        Some(e) if e.holder == holder => {
          e.until = Instant::now() + ttl;
          Ok(())
        }
        Some(e) => Err(SingletonError::HeldBy(e.holder.clone())),
        None => Err(SingletonError::Unavailable("no lease".into())),
      }
    })
  }

  fn release(&self, name: &str, holder: &str) -> Effect<(), SingletonError> {
    let (name, holder) = (name.to_string(), holder.to_string());
    self.with_table(move |map| match map.get(&name) {
      Some(e) if e.holder == holder => {
        map.remove(&name);
        Ok(())
      }
      Some(e) => Err(SingletonError::HeldBy(e.holder.clone())),
      None => Ok(()),
    })
  }

  fn holder(&self, name: &str) -> Effect<Option<String>, Never> {
    let name = name.to_string();
    self.with_table(move |map| {
      Self::sweep(map);
      Ok(map.get(&name).map(|e| e.holder.clone()))
    })
  }
}

/// Cluster singleton handle: lease + optional local pid for the holder.
pub struct ClusterSingleton {
  name: String,
  holder_id: String,
  lease: Arc<dyn SingletonLease>,
  ttl: Duration,
  local_pid: Arc<Mutex<Option<Pid>>>,
}

impl ClusterSingleton {
  /// Bind to a lease backend.
  pub fn new(
    name: impl Into<String>,
    holder_id: impl Into<String>,
    lease: Arc<dyn SingletonLease>,
    ttl: Duration,
  ) -> Self {
    Self {
      name: name.into(),
      holder_id: holder_id.into(),
      lease,
      ttl,
      local_pid: Arc::new(Mutex::new(None)),
    }
  }

  /// Attempt to become the singleton; records `pid` on success.
  pub fn try_become(&self, pid: &Pid) -> Effect<(), SingletonError> {
    let (slot, pid) = (Arc::clone(&self.local_pid), pid.clone());
    self
      .lease
      .try_acquire(&self.name, &self.holder_id, self.ttl)
      .flat_map(move |()| sync(move || *slot.lock().expect("pid") = Some(pid)))
  }

  /// Renew the lease (run it from a heartbeat fiber, e.g. under a `Schedule`).
  pub fn heartbeat(&self) -> Effect<(), SingletonError> {
    self.lease.renew(&self.name, &self.holder_id, self.ttl)
  }

  /// Step down.
  pub fn resign(&self) -> Effect<(), SingletonError> {
    let slot = Arc::clone(&self.local_pid);
    self
      .lease
      .release(&self.name, &self.holder_id)
      .flat_map(move |()| sync(move || *slot.lock().expect("pid") = None))
  }

  /// Whether this node currently holds the singleton.
  pub fn is_holder(&self) -> Effect<bool, Never> {
    let me = self.holder_id.clone();
    self
      .lease
      .holder(&self.name)
      .map(move |h| h.as_deref() == Some(me.as_str()))
  }

  /// Local process running the singleton role, if we are the holder.
  pub fn local_pid(&self) -> Option<Pid> {
    self.local_pid.lock().expect("pid").clone()
  }
}

// ---------------------------------------------------------------------------
// Postgres advisory-lock lease (feature `pg`)
// ---------------------------------------------------------------------------

/// Command sent from a [`PgAdvisoryLease`] handle to its dedicated-session worker.
#[cfg(feature = "pg")]
enum LeaseCmd {
  Acquire {
    k1: i32,
    k2: i32,
    resp: flume::Sender<Result<bool, String>>,
  },
  Unlock {
    k1: i32,
    k2: i32,
    resp: flume::Sender<Result<bool, String>>,
  },
  Ping {
    resp: flume::Sender<Result<(), String>>,
  },
}

/// The dedicated-session worker: a daemon fiber that owns a single Postgres connection and
/// runs advisory-lock SQL on it.
///
/// The lock is session-scoped: it stays held for exactly as long as this fiber (and therefore
/// the connection) lives. When the [`PgAdvisoryLease`] handle drops, the fiber is interrupted
/// (and `commands` closes), the connection closes, and Postgres releases all advisory locks —
/// the failover trigger.
#[cfg(feature = "pg")]
fn lease_worker(
  url: String,
  commands: flume::Receiver<LeaseCmd>,
  ready: flume::Sender<Result<(), String>>,
) -> Effect<(), Never> {
  id_effect::next::from_async(move || async move {
    use sqlx::Connection as _;

    let mut conn = match sqlx::postgres::PgConnection::connect(&url).await {
      Ok(conn) => conn,
      Err(e) => {
        let _ = ready.send(Err(format!("connect: {e}")));
        return Ok(());
      }
    };
    let _ = ready.send(Ok(()));

    while let Ok(cmd) = commands.recv_async().await {
      match cmd {
        LeaseCmd::Acquire { k1, k2, resp } => {
          let outcome = sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1, $2)")
            .bind(k1)
            .bind(k2)
            .fetch_one(&mut conn)
            .await
            .map_err(|e| e.to_string());
          let _ = resp.send(outcome);
        }
        LeaseCmd::Unlock { k1, k2, resp } => {
          let outcome = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock($1, $2)")
            .bind(k1)
            .bind(k2)
            .fetch_one(&mut conn)
            .await
            .map_err(|e| e.to_string());
          let _ = resp.send(outcome);
        }
        LeaseCmd::Ping { resp } => {
          let outcome = sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&mut conn)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string());
          let _ = resp.send(outcome);
        }
      }
    }
    // `conn` drops here: session ends, advisory locks released.
    Ok(())
  })
}

/// Postgres session-level advisory-lock [`SingletonLease`] — the CP production path (ADR 0009).
///
/// Holds a **dedicated** connection on a daemon fiber for the lease lifetime so
/// `pg_try_advisory_lock` stays session-scoped and is auto-released on connection loss or drop,
/// enabling failover. TTLs are ignored: ownership tracks session liveness, not wall-clock time.
///
/// Each node process should hold its own lease (one connection). Enabled by the `pg` feature;
/// construct via [`PgAdvisoryLease::connect`] or [`PgAdvisoryLease::from_pool_config`] on a
/// `TokioExecutor` over a tokio runtime with the IO driver enabled (sqlx needs it); elsewhere
/// the connect fails with [`SingletonError::Unavailable`] naming the session fiber's defect.
#[cfg(feature = "pg")]
pub struct PgAdvisoryLease {
  commands: flume::Sender<LeaseCmd>,
  /// Names this session holds → holder id. An async mutex: an operation holds it across its
  /// round-trip to the worker, so check-then-lock is atomic per lease.
  held: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
  /// The session fiber; interrupted when the handle drops.
  worker: Option<id_effect::next::FiberHandle<(), Never>>,
}

#[cfg(feature = "pg")]
impl Drop for PgAdvisoryLease {
  fn drop(&mut self) {
    if let Some(worker) = &self.worker {
      worker.interrupt_now();
    }
  }
}

#[cfg(feature = "pg")]
fn worker_gone() -> SingletonError {
  SingletonError::Unavailable("lease worker gone".into())
}

#[cfg(feature = "pg")]
fn response_dropped() -> SingletonError {
  SingletonError::Unavailable("lease worker dropped response".into())
}

#[cfg(feature = "pg")]
async fn round_trip<T>(
  commands: &flume::Sender<LeaseCmd>,
  cmd: impl FnOnce(flume::Sender<Result<T, String>>) -> LeaseCmd,
) -> Result<T, SingletonError> {
  let (tx, rx) = flume::bounded(1);
  commands.send(cmd(tx)).map_err(|_| worker_gone())?;
  rx.recv_async()
    .await
    .map_err(|_| response_dropped())?
    .map_err(SingletonError::Unavailable)
}

#[cfg(feature = "pg")]
async fn ping(commands: &flume::Sender<LeaseCmd>) -> Result<(), SingletonError> {
  round_trip(commands, |resp| LeaseCmd::Ping { resp }).await
}

#[cfg(feature = "pg")]
impl PgAdvisoryLease {
  /// Open a dedicated advisory-lock session at `url` (`postgres://…`).
  ///
  /// Forks the session fiber and completes once the connection is established; fails with
  /// [`SingletonError::Unavailable`] if it cannot connect.
  pub fn connect(url: impl Into<String>) -> Effect<Self, SingletonError> {
    let url = url.into();
    suspend(move || {
      let (commands, rx) = flume::unbounded();
      let (ready_tx, ready_rx) = flume::bounded(1);
      lease_worker(url, rx, ready_tx)
        .fork_daemon()
        .widen()
        .flat_map(move |worker| {
          id_effect::next::from_async(move || async move {
            Ok::<_, SingletonError>(ready_rx.recv_async().await.ok())
          })
          .flat_map(move |ready| match ready {
            Some(Ok(())) => succeed(Self {
              commands,
              held: Arc::default(),
              worker: Some(worker),
            }),
            Some(Err(e)) => fail(SingletonError::Unavailable(e)),
            // The session fiber died before reporting (e.g. sqlx found no tokio IO driver).
            None => worker.join().exit().flat_map(|exit: Exit<(), Never>| {
              let why = match exit {
                Exit::Failure(Cause::Die(d)) => d.message().to_string(),
                other => format!("{other:?}"),
              };
              fail(SingletonError::Unavailable(format!(
                "lease worker exited before ready: {why}"
              )))
            }),
          })
        })
    })
  }

  /// Open a lease using an [`id_effect_sql::pg::PgPoolConfig`]'s connection URL.
  pub fn from_pool_config(config: id_effect_sql::pg::PgPoolConfig) -> Effect<Self, SingletonError> {
    Self::connect(config.url)
  }

  /// Run `f` with the session's command channel and held-names table (locked).
  fn op<A, F, Fut>(&self, f: F) -> Effect<A, SingletonError>
  where
    A: Send + 'static,
    F: FnOnce(flume::Sender<LeaseCmd>, Arc<tokio::sync::Mutex<HashMap<String, String>>>) -> Fut
      + Send
      + 'static,
    Fut: std::future::Future<Output = Result<A, SingletonError>> + Send + 'static,
  {
    let (commands, held) = (self.commands.clone(), Arc::clone(&self.held));
    id_effect::next::from_async(move || f(commands, held))
  }
}

#[cfg(feature = "pg")]
impl SingletonLease for PgAdvisoryLease {
  fn try_acquire(&self, name: &str, holder: &str, _ttl: Duration) -> Effect<(), SingletonError> {
    let (name, holder) = (name.to_string(), holder.to_string());
    self.op(move |commands, held| async move {
      let mut held = held.lock().await;
      if let Some(existing) = held.get(&name) {
        return if *existing == holder {
          Ok(())
        } else {
          Err(SingletonError::HeldBy(existing.clone()))
        };
      }
      let (k1, k2) = advisory_lock_keys(&name);
      if round_trip(&commands, |resp| LeaseCmd::Acquire { k1, k2, resp }).await? {
        held.insert(name, holder);
        Ok(())
      } else {
        Err(SingletonError::HeldBy("another session".into()))
      }
    })
  }

  fn renew(&self, name: &str, holder: &str, _ttl: Duration) -> Effect<(), SingletonError> {
    let (name, holder) = (name.to_string(), holder.to_string());
    self.op(move |commands, held| async move {
      {
        let held = held.lock().await;
        match held.get(&name) {
          Some(h) if *h == holder => {}
          Some(h) => return Err(SingletonError::HeldBy(h.clone())),
          None => return Err(SingletonError::Unavailable("no lease".into())),
        }
      }
      // Ownership is session liveness: a failed ping means the session (and lock) is gone.
      ping(&commands).await
    })
  }

  fn release(&self, name: &str, holder: &str) -> Effect<(), SingletonError> {
    let (name, holder) = (name.to_string(), holder.to_string());
    self.op(move |commands, held| async move {
      let mut held = held.lock().await;
      match held.get(&name) {
        Some(h) if *h == holder => {
          let (k1, k2) = advisory_lock_keys(&name);
          let _ = round_trip(&commands, |resp| LeaseCmd::Unlock { k1, k2, resp }).await;
          held.remove(&name);
          Ok(())
        }
        Some(h) => Err(SingletonError::HeldBy(h.clone())),
        None => Ok(()),
      }
    })
  }

  fn holder(&self, name: &str) -> Effect<Option<String>, Never> {
    let name = name.to_string();
    let (commands, held) = (self.commands.clone(), Arc::clone(&self.held));
    id_effect::next::from_async(move || async move {
      let owner = held.lock().await.get(&name).cloned();
      // Confirm the session is still alive; otherwise the lock has already been lost.
      Ok(match owner {
        Some(owner) if ping(&commands).await.is_ok() => Some(owner),
        _ => None,
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::{Cause, Exit};
  use id_effect_process::NodeName;

  /// Run a lease effect to its typed outcome.
  fn run<A: Send + 'static>(eff: Effect<A, SingletonError>) -> Result<A, SingletonError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  /// Run an infallible lease effect.
  fn get<A: Send + 'static>(eff: Effect<A, Never>) -> A {
    match eff.block_on() {
      Exit::Success(a) => a,
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  #[test]
  fn only_one_holder() {
    let lease = Arc::new(MemoryLease::new());
    let a = ClusterSingleton::new(
      "leader",
      "a",
      Arc::clone(&lease) as _,
      Duration::from_secs(5),
    );
    let b = ClusterSingleton::new(
      "leader",
      "b",
      Arc::clone(&lease) as _,
      Duration::from_secs(5),
    );
    let pid = Pid::from_parts(NodeName::new("a@t"), 1, 1);
    run(a.try_become(&pid)).unwrap();
    assert!(get(a.is_holder()));
    assert!(matches!(
      run(b.try_become(&pid)),
      Err(SingletonError::HeldBy(_))
    ));
    run(a.resign()).unwrap();
    run(b.try_become(&pid)).unwrap();
    assert!(get(b.is_holder()));
  }

  const LONG: Duration = Duration::from_secs(3600);

  fn pid() -> Pid {
    Pid::from_parts(NodeName::new("a@t"), 1, 1)
  }

  #[test]
  fn acquire_by_the_same_holder_is_reentrant() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("job", "a", LONG)).expect("first");
    run(lease.try_acquire("job", "a", LONG)).expect("re-acquire");
    assert_eq!(get(lease.holder("job")).as_deref(), Some("a"));
  }

  #[test]
  fn contender_learns_who_holds_the_lease() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("job", "a", LONG)).expect("a");
    assert_eq!(
      run(lease.try_acquire("job", "b", LONG)),
      Err(SingletonError::HeldBy("a".into()))
    );
  }

  #[test]
  fn leases_are_independent_per_name() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("one", "a", LONG)).expect("one");
    run(lease.try_acquire("two", "b", LONG)).expect("two");
    assert_eq!(get(lease.holder("one")).as_deref(), Some("a"));
    assert_eq!(get(lease.holder("two")).as_deref(), Some("b"));
  }

  #[test]
  fn renew_keeps_the_holder() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("job", "a", LONG)).expect("acquire");
    run(lease.renew("job", "a", LONG)).expect("renew");
    assert_eq!(get(lease.holder("job")).as_deref(), Some("a"));
  }

  #[test]
  fn renew_by_a_non_holder_is_refused() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("job", "a", LONG)).expect("acquire");
    assert_eq!(
      run(lease.renew("job", "b", LONG)),
      Err(SingletonError::HeldBy("a".into()))
    );
  }

  #[test]
  fn renew_without_a_lease_is_unavailable() {
    let lease = MemoryLease::new();
    assert!(matches!(
      run(lease.renew("job", "a", LONG)),
      Err(SingletonError::Unavailable(_))
    ));
  }

  #[test]
  fn release_by_a_non_holder_is_refused_and_keeps_the_lease() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("job", "a", LONG)).expect("acquire");
    assert_eq!(
      run(lease.release("job", "b")),
      Err(SingletonError::HeldBy("a".into()))
    );
    assert_eq!(get(lease.holder("job")).as_deref(), Some("a"));
  }

  #[test]
  fn release_of_an_absent_lease_is_ok() {
    assert_eq!(run(MemoryLease::new().release("job", "a")), Ok(()));
  }

  // A zero TTL is expired the moment it is granted (deadline == grant
  // instant, and the monotonic clock never runs backwards), so expiry is
  // tested without sleeping.
  #[test]
  fn expired_lease_has_no_holder_and_can_be_taken_over() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("job", "a", Duration::ZERO)).expect("a");
    assert_eq!(get(lease.holder("job")), None);
    run(lease.try_acquire("job", "b", LONG)).expect("b takes over");
    assert_eq!(get(lease.holder("job")).as_deref(), Some("b"));
  }

  #[test]
  fn expired_lease_cannot_be_renewed() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("job", "a", Duration::ZERO)).expect("a");
    assert!(matches!(
      run(lease.renew("job", "a", LONG)),
      Err(SingletonError::Unavailable(_))
    ));
  }

  #[test]
  fn renew_extends_a_lease_that_would_have_expired() {
    let lease = MemoryLease::new();
    run(lease.try_acquire("job", "a", LONG)).expect("a");
    run(lease.renew("job", "a", Duration::ZERO)).expect("shrink");
    assert_eq!(
      get(lease.holder("job")),
      None,
      "renewal sets a new deadline"
    );
  }

  #[test]
  fn singleton_tracks_its_local_pid() {
    let lease: Arc<dyn SingletonLease> = Arc::new(MemoryLease::new());
    let single = ClusterSingleton::new("leader", "a", lease, LONG);
    assert_eq!(single.local_pid(), None);
    assert!(!get(single.is_holder()));
    run(single.try_become(&pid())).expect("become");
    assert_eq!(single.local_pid(), Some(pid()));
    run(single.heartbeat()).expect("heartbeat");
    run(single.resign()).expect("resign");
    assert_eq!(single.local_pid(), None);
    assert!(!get(single.is_holder()));
  }

  #[test]
  fn failed_takeover_records_no_pid() {
    let lease = Arc::new(MemoryLease::new());
    let a = ClusterSingleton::new("leader", "a", Arc::clone(&lease) as _, LONG);
    let b = ClusterSingleton::new("leader", "b", lease as _, LONG);
    run(a.try_become(&pid())).expect("a");
    assert!(run(b.try_become(&pid())).is_err());
    assert_eq!(b.local_pid(), None);
    assert!(matches!(run(b.heartbeat()), Err(SingletonError::HeldBy(h)) if h == "a"));
  }

  #[test]
  fn singleton_error_display() {
    assert_eq!(SingletonError::HeldBy("a".into()).to_string(), "held by a");
    assert_eq!(
      SingletonError::Unavailable("pool down".into()).to_string(),
      "unavailable: pool down"
    );
    let boxed: Box<dyn std::error::Error> = Box::new(SingletonError::HeldBy("a".into()));
    assert_eq!(boxed.to_string(), "held by a");
  }

  #[test]
  fn advisory_keys_are_pinned_to_sha256() {
    // Golden values: SHA-256("leader") = 4d5b6609 43625c0a …
    assert_eq!(advisory_lock_keys("leader"), (0x4d5b_6609, 0x4362_5c0a));
    // SHA-256("") = e3b0c442 98fc1c14 … (high bit set → negative i32).
    assert_eq!(
      advisory_lock_keys(""),
      (0xe3b0_c442_u32 as i32, 0x98fc_1c14_u32 as i32)
    );
  }

  #[test]
  fn advisory_keys_differ_per_name() {
    assert_ne!(advisory_lock_keys("leader"), advisory_lock_keys("leader2"));
  }

  // No database needed: an unparseable URL fails inside the worker before any I/O, so the
  // connect error is reported through the ready channel deterministically.
  #[cfg(feature = "pg")]
  #[test]
  fn pg_lease_connect_failure_is_unavailable() {
    match run(PgAdvisoryLease::connect("not-a-postgres-url")) {
      Err(SingletonError::Unavailable(msg)) => assert!(msg.starts_with("connect:"), "{msg}"),
      Err(other) => panic!("unexpected error: {other}"),
      Ok(_) => panic!("connect to a bogus url must fail"),
    }
  }

  // The lease bookkeeping (held map, ping-based liveness, error mapping) is exercised against a
  // scripted worker that speaks the same `LeaseCmd` protocol; the SQL itself is covered by the
  // `singleton_pg` integration test against a live database.
  #[cfg(feature = "pg")]
  mod pg_protocol {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Clone, Default)]
    struct Script {
      /// Keys another session already holds (`pg_try_advisory_lock` returns false).
      contested: Vec<(i32, i32)>,
      /// SQL error returned for every acquire.
      acquire_err: Option<String>,
      /// Drop response senders instead of answering.
      drop_responses: bool,
    }

    struct Fake {
      lease: PgAdvisoryLease,
      session_alive: Arc<AtomicBool>,
      unlocked: Arc<Mutex<Vec<(i32, i32)>>>,
    }

    fn fake(script: Script) -> Fake {
      let (commands, rx) = flume::unbounded::<LeaseCmd>();
      let session_alive = Arc::new(AtomicBool::new(true));
      let unlocked = Arc::new(Mutex::new(Vec::new()));
      let (alive, log) = (Arc::clone(&session_alive), Arc::clone(&unlocked));
      std::thread::spawn(move || {
        while let Ok(cmd) = rx.recv() {
          if script.drop_responses {
            continue;
          }
          match cmd {
            LeaseCmd::Acquire { k1, k2, resp } => {
              let outcome = match &script.acquire_err {
                Some(e) => Err(e.clone()),
                None => Ok(!script.contested.contains(&(k1, k2))),
              };
              let _ = resp.send(outcome);
            }
            LeaseCmd::Unlock { k1, k2, resp } => {
              log.lock().unwrap().push((k1, k2));
              let _ = resp.send(Ok(true));
            }
            LeaseCmd::Ping { resp } => {
              let outcome = if alive.load(Ordering::SeqCst) {
                Ok(())
              } else {
                Err("connection reset".to_string())
              };
              let _ = resp.send(outcome);
            }
          }
        }
      });
      Fake {
        lease: PgAdvisoryLease {
          commands,
          held: Arc::default(),
          worker: None,
        },
        session_alive,
        unlocked,
      }
    }

    fn worker_gone() -> PgAdvisoryLease {
      let (commands, rx) = flume::unbounded::<LeaseCmd>();
      drop(rx);
      PgAdvisoryLease {
        commands,
        held: Arc::new(tokio::sync::Mutex::new(HashMap::from([(
          "job".to_string(),
          "a".to_string(),
        )]))),
        worker: None,
      }
    }

    #[test]
    fn acquire_is_reentrant_and_exclusive_per_process() {
      let f = fake(Script::default());
      run(f.lease.try_acquire("job", "a", LONG)).expect("acquire");
      run(f.lease.try_acquire("job", "a", LONG)).expect("re-acquire");
      assert_eq!(
        run(f.lease.try_acquire("job", "b", LONG)),
        Err(SingletonError::HeldBy("a".into()))
      );
      assert_eq!(get(f.lease.holder("job")).as_deref(), Some("a"));
      assert_eq!(get(f.lease.holder("other")), None);
    }

    #[test]
    fn lock_held_by_another_session_is_refused() {
      let f = fake(Script {
        contested: vec![advisory_lock_keys("job")],
        ..Script::default()
      });
      assert_eq!(
        run(f.lease.try_acquire("job", "a", LONG)),
        Err(SingletonError::HeldBy("another session".into()))
      );
      assert_eq!(get(f.lease.holder("job")), None);
      run(f.lease.try_acquire("free", "a", LONG)).expect("other key");
    }

    #[test]
    fn acquire_sql_error_is_unavailable() {
      let f = fake(Script {
        acquire_err: Some("permission denied".into()),
        ..Script::default()
      });
      assert_eq!(
        run(f.lease.try_acquire("job", "a", LONG)),
        Err(SingletonError::Unavailable("permission denied".into()))
      );
    }

    #[test]
    fn renew_tracks_session_liveness() {
      let f = fake(Script::default());
      assert_eq!(
        run(f.lease.renew("job", "a", LONG)),
        Err(SingletonError::Unavailable("no lease".into()))
      );
      run(f.lease.try_acquire("job", "a", LONG)).expect("acquire");
      run(f.lease.renew("job", "a", LONG)).expect("renew");
      assert_eq!(
        run(f.lease.renew("job", "b", LONG)),
        Err(SingletonError::HeldBy("a".into()))
      );
      f.session_alive.store(false, Ordering::SeqCst);
      assert_eq!(
        run(f.lease.renew("job", "a", LONG)),
        Err(SingletonError::Unavailable("connection reset".into()))
      );
      assert_eq!(
        get(f.lease.holder("job")),
        None,
        "a dead session holds nothing"
      );
    }

    #[test]
    fn release_unlocks_only_for_the_holder() {
      let f = fake(Script::default());
      assert_eq!(run(f.lease.release("job", "a")), Ok(()));
      run(f.lease.try_acquire("job", "a", LONG)).expect("acquire");
      assert_eq!(
        run(f.lease.release("job", "b")),
        Err(SingletonError::HeldBy("a".into()))
      );
      assert!(f.unlocked.lock().unwrap().is_empty());
      run(f.lease.release("job", "a")).expect("release");
      assert_eq!(*f.unlocked.lock().unwrap(), vec![advisory_lock_keys("job")]);
      run(f.lease.try_acquire("job", "b", LONG)).expect("b after release");
    }

    #[test]
    fn missing_worker_is_unavailable() {
      let lease = worker_gone();
      let gone = SingletonError::Unavailable("lease worker gone".into());
      assert_eq!(
        run(lease.try_acquire("other", "a", LONG)),
        Err(gone.clone())
      );
      assert_eq!(run(lease.renew("job", "a", LONG)), Err(gone));
      assert_eq!(get(lease.holder("job")), None);
    }

    #[test]
    fn dropped_response_is_unavailable() {
      let f = fake(Script {
        drop_responses: true,
        ..Script::default()
      });
      let dropped = SingletonError::Unavailable("lease worker dropped response".into());
      assert_eq!(
        run(f.lease.try_acquire("job", "a", LONG)),
        Err(dropped.clone())
      );
      f.lease
        .held
        .blocking_lock()
        .insert("job".into(), "a".into());
      assert_eq!(run(f.lease.renew("job", "a", LONG)), Err(dropped));
      // Release still clears local state even if the unlock reply is lost.
      assert_eq!(run(f.lease.release("job", "a")), Ok(()));
      assert!(f.lease.held.blocking_lock().is_empty());
    }
  }

  #[cfg(feature = "pg")]
  #[test]
  fn pg_lease_from_pool_config_uses_its_url() {
    let config = id_effect_sql::pg::PgPoolConfig::from_url("not-a-postgres-url");
    assert!(matches!(
      run(PgAdvisoryLease::from_pool_config(config)),
      Err(SingletonError::Unavailable(_))
    ));
  }
}
