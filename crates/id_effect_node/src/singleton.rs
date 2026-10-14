//! CP [`ClusterSingleton`] — at most one holder cluster-wide.
//!
//! Netsplit story (ADR 0009): unavailable without quorum. Production path
//! uses Postgres advisory-lock leases (`pg` feature via `id_effect_sql` (`pg` feature)).
//! This module ships an **in-memory lease** suitable for single-process
//! tests and as the interface the PG backend implements.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// Abstract lease backend.
pub trait SingletonLease: Send + Sync {
  /// Try to acquire `name` for `holder` until `ttl` elapses.
  fn try_acquire(&self, name: &str, holder: &str, ttl: Duration) -> Result<(), SingletonError>;

  /// Renew an existing lease (must already be held by `holder`).
  fn renew(&self, name: &str, holder: &str, ttl: Duration) -> Result<(), SingletonError>;

  /// Release if (and only if) `holder` owns it.
  fn release(&self, name: &str, holder: &str) -> Result<(), SingletonError>;

  /// Current holder id, if any and not expired.
  fn holder(&self, name: &str) -> Option<String>;
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
}

impl SingletonLease for MemoryLease {
  fn try_acquire(&self, name: &str, holder: &str, ttl: Duration) -> Result<(), SingletonError> {
    let mut map = self.inner.lock().expect("lease");
    Self::sweep(&mut map);
    match map.get(name) {
      Some(e) if e.holder != holder => Err(SingletonError::HeldBy(e.holder.clone())),
      _ => {
        map.insert(
          name.to_string(),
          MemEntry {
            holder: holder.to_string(),
            until: Instant::now() + ttl,
          },
        );
        Ok(())
      }
    }
  }

  fn renew(&self, name: &str, holder: &str, ttl: Duration) -> Result<(), SingletonError> {
    let mut map = self.inner.lock().expect("lease");
    Self::sweep(&mut map);
    match map.get_mut(name) {
      Some(e) if e.holder == holder => {
        e.until = Instant::now() + ttl;
        Ok(())
      }
      Some(e) => Err(SingletonError::HeldBy(e.holder.clone())),
      None => Err(SingletonError::Unavailable("no lease".into())),
    }
  }

  fn release(&self, name: &str, holder: &str) -> Result<(), SingletonError> {
    let mut map = self.inner.lock().expect("lease");
    match map.get(name) {
      Some(e) if e.holder == holder => {
        map.remove(name);
        Ok(())
      }
      Some(e) => Err(SingletonError::HeldBy(e.holder.clone())),
      None => Ok(()),
    }
  }

  fn holder(&self, name: &str) -> Option<String> {
    let mut map = self.inner.lock().expect("lease");
    Self::sweep(&mut map);
    map.get(name).map(|e| e.holder.clone())
  }
}

/// Cluster singleton handle: lease + optional local pid for the holder.
pub struct ClusterSingleton {
  name: String,
  holder_id: String,
  lease: Arc<dyn SingletonLease>,
  ttl: Duration,
  local_pid: Mutex<Option<Pid>>,
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
      local_pid: Mutex::new(None),
    }
  }

  /// Attempt to become the singleton; records `pid` on success.
  pub fn try_become(&self, pid: &Pid) -> Result<(), SingletonError> {
    self
      .lease
      .try_acquire(&self.name, &self.holder_id, self.ttl)?;
    *self.local_pid.lock().expect("pid") = Some(pid.clone());
    Ok(())
  }

  /// Renew the lease (call from a heartbeat fiber).
  pub fn heartbeat(&self) -> Result<(), SingletonError> {
    self.lease.renew(&self.name, &self.holder_id, self.ttl)
  }

  /// Step down.
  pub fn resign(&self) -> Result<(), SingletonError> {
    self.lease.release(&self.name, &self.holder_id)?;
    *self.local_pid.lock().expect("pid") = None;
    Ok(())
  }

  /// Whether this node currently holds the singleton.
  pub fn is_holder(&self) -> bool {
    self.lease.holder(&self.name).as_deref() == Some(self.holder_id.as_str())
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

/// Owns a single Postgres connection and runs advisory-lock SQL on it.
///
/// The lock is session-scoped: it stays held for exactly as long as this loop (and therefore the
/// connection) lives. When every [`PgAdvisoryLease`] handle drops, `commands` closes, the loop
/// ends, the connection closes, and Postgres releases all advisory locks — the failover trigger.
#[cfg(feature = "pg")]
fn run_lease_worker(
  url: String,
  commands: flume::Receiver<LeaseCmd>,
  ready: flume::Sender<Result<(), String>>,
) {
  use sqlx::Connection as _;

  let runtime = match tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
  {
    Ok(rt) => rt,
    Err(e) => {
      let _ = ready.send(Err(format!("tokio runtime: {e}")));
      return;
    }
  };

  runtime.block_on(async move {
    let mut conn = match sqlx::postgres::PgConnection::connect(&url).await {
      Ok(conn) => conn,
      Err(e) => {
        let _ = ready.send(Err(format!("connect: {e}")));
        return;
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
  });
}

/// Postgres session-level advisory-lock [`SingletonLease`] — the CP production path (ADR 0009).
///
/// Holds a **dedicated** connection on a worker thread for the lease lifetime so
/// `pg_try_advisory_lock` stays session-scoped and is auto-released on connection loss or drop,
/// enabling failover. TTLs are ignored: ownership tracks session liveness, not wall-clock time.
///
/// Each node process should hold its own lease (one connection). Enabled by the `pg` feature;
/// construct via [`PgAdvisoryLease::connect`] or [`PgAdvisoryLease::from_pool_config`].
#[cfg(feature = "pg")]
pub struct PgAdvisoryLease {
  commands: flume::Sender<LeaseCmd>,
  held: Mutex<HashMap<String, String>>,
}

#[cfg(feature = "pg")]
impl PgAdvisoryLease {
  /// Open a dedicated advisory-lock session at `url` (`postgres://…`).
  ///
  /// Blocks until the connection is established; returns [`SingletonError::Unavailable`] if the
  /// worker cannot connect.
  pub fn connect(url: impl Into<String>) -> Result<Self, SingletonError> {
    let url = url.into();
    let (commands, rx) = flume::unbounded();
    let (ready_tx, ready_rx) = flume::bounded(1);
    std::thread::Builder::new()
      .name("pg-advisory-lease".into())
      .spawn(move || run_lease_worker(url, rx, ready_tx))
      .map_err(|e| SingletonError::Unavailable(format!("spawn lease worker: {e}")))?;
    match ready_rx.recv() {
      Ok(Ok(())) => Ok(Self {
        commands,
        held: Mutex::new(HashMap::new()),
      }),
      Ok(Err(e)) => Err(SingletonError::Unavailable(e)),
      Err(_) => Err(SingletonError::Unavailable(
        "lease worker exited before ready".into(),
      )),
    }
  }

  /// Open a lease using an [`id_effect_sql::pg::PgPoolConfig`]'s connection URL.
  pub fn from_pool_config(config: id_effect_sql::pg::PgPoolConfig) -> Result<Self, SingletonError> {
    Self::connect(config.url)
  }

  fn command_bool(
    &self,
    cmd: LeaseCmd,
    rx: flume::Receiver<Result<bool, String>>,
  ) -> Result<bool, SingletonError> {
    self
      .commands
      .send(cmd)
      .map_err(|_| SingletonError::Unavailable("lease worker gone".into()))?;
    rx.recv()
      .map_err(|_| SingletonError::Unavailable("lease worker dropped response".into()))?
      .map_err(SingletonError::Unavailable)
  }

  fn acquire_lock(&self, k1: i32, k2: i32) -> Result<bool, SingletonError> {
    let (tx, rx) = flume::bounded(1);
    self.command_bool(LeaseCmd::Acquire { k1, k2, resp: tx }, rx)
  }

  fn unlock(&self, k1: i32, k2: i32) -> Result<bool, SingletonError> {
    let (tx, rx) = flume::bounded(1);
    self.command_bool(LeaseCmd::Unlock { k1, k2, resp: tx }, rx)
  }

  fn ping(&self) -> Result<(), SingletonError> {
    let (tx, rx) = flume::bounded(1);
    self
      .commands
      .send(LeaseCmd::Ping { resp: tx })
      .map_err(|_| SingletonError::Unavailable("lease worker gone".into()))?;
    rx.recv()
      .map_err(|_| SingletonError::Unavailable("lease worker dropped response".into()))?
      .map_err(SingletonError::Unavailable)
  }
}

#[cfg(feature = "pg")]
impl SingletonLease for PgAdvisoryLease {
  fn try_acquire(&self, name: &str, holder: &str, _ttl: Duration) -> Result<(), SingletonError> {
    let mut held = self
      .held
      .lock()
      .map_err(|_| SingletonError::Unavailable("lease state poisoned".into()))?;
    if let Some(existing) = held.get(name) {
      return if existing == holder {
        Ok(())
      } else {
        Err(SingletonError::HeldBy(existing.clone()))
      };
    }
    let (k1, k2) = advisory_lock_keys(name);
    if self.acquire_lock(k1, k2)? {
      held.insert(name.to_string(), holder.to_string());
      Ok(())
    } else {
      Err(SingletonError::HeldBy("another session".into()))
    }
  }

  fn renew(&self, name: &str, holder: &str, _ttl: Duration) -> Result<(), SingletonError> {
    {
      let held = self
        .held
        .lock()
        .map_err(|_| SingletonError::Unavailable("lease state poisoned".into()))?;
      match held.get(name) {
        Some(h) if h == holder => {}
        Some(h) => return Err(SingletonError::HeldBy(h.clone())),
        None => return Err(SingletonError::Unavailable("no lease".into())),
      }
    }
    // Ownership is session liveness: a failed ping means the session (and lock) is gone.
    self.ping()
  }

  fn release(&self, name: &str, holder: &str) -> Result<(), SingletonError> {
    let mut held = self
      .held
      .lock()
      .map_err(|_| SingletonError::Unavailable("lease state poisoned".into()))?;
    match held.get(name) {
      Some(h) if h == holder => {
        let (k1, k2) = advisory_lock_keys(name);
        let _ = self.unlock(k1, k2);
        held.remove(name);
        Ok(())
      }
      Some(h) => Err(SingletonError::HeldBy(h.clone())),
      None => Ok(()),
    }
  }

  fn holder(&self, name: &str) -> Option<String> {
    let owner = {
      let held = self.held.lock().ok()?;
      held.get(name)?.clone()
    };
    // Confirm the session is still alive; otherwise the lock has already been lost.
    if self.ping().is_ok() {
      Some(owner)
    } else {
      None
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_process::NodeName;

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
    a.try_become(&pid).unwrap();
    assert!(a.is_holder());
    assert!(matches!(b.try_become(&pid), Err(SingletonError::HeldBy(_))));
    a.resign().unwrap();
    b.try_become(&pid).unwrap();
    assert!(b.is_holder());
  }

  const LONG: Duration = Duration::from_secs(3600);

  fn pid() -> Pid {
    Pid::from_parts(NodeName::new("a@t"), 1, 1)
  }

  #[test]
  fn acquire_by_the_same_holder_is_reentrant() {
    let lease = MemoryLease::new();
    lease.try_acquire("job", "a", LONG).expect("first");
    lease.try_acquire("job", "a", LONG).expect("re-acquire");
    assert_eq!(lease.holder("job").as_deref(), Some("a"));
  }

  #[test]
  fn contender_learns_who_holds_the_lease() {
    let lease = MemoryLease::new();
    lease.try_acquire("job", "a", LONG).expect("a");
    assert_eq!(
      lease.try_acquire("job", "b", LONG),
      Err(SingletonError::HeldBy("a".into()))
    );
  }

  #[test]
  fn leases_are_independent_per_name() {
    let lease = MemoryLease::new();
    lease.try_acquire("one", "a", LONG).expect("one");
    lease.try_acquire("two", "b", LONG).expect("two");
    assert_eq!(lease.holder("one").as_deref(), Some("a"));
    assert_eq!(lease.holder("two").as_deref(), Some("b"));
  }

  #[test]
  fn renew_keeps_the_holder() {
    let lease = MemoryLease::new();
    lease.try_acquire("job", "a", LONG).expect("acquire");
    lease.renew("job", "a", LONG).expect("renew");
    assert_eq!(lease.holder("job").as_deref(), Some("a"));
  }

  #[test]
  fn renew_by_a_non_holder_is_refused() {
    let lease = MemoryLease::new();
    lease.try_acquire("job", "a", LONG).expect("acquire");
    assert_eq!(
      lease.renew("job", "b", LONG),
      Err(SingletonError::HeldBy("a".into()))
    );
  }

  #[test]
  fn renew_without_a_lease_is_unavailable() {
    let lease = MemoryLease::new();
    assert!(matches!(
      lease.renew("job", "a", LONG),
      Err(SingletonError::Unavailable(_))
    ));
  }

  #[test]
  fn release_by_a_non_holder_is_refused_and_keeps_the_lease() {
    let lease = MemoryLease::new();
    lease.try_acquire("job", "a", LONG).expect("acquire");
    assert_eq!(
      lease.release("job", "b"),
      Err(SingletonError::HeldBy("a".into()))
    );
    assert_eq!(lease.holder("job").as_deref(), Some("a"));
  }

  #[test]
  fn release_of_an_absent_lease_is_ok() {
    assert_eq!(MemoryLease::new().release("job", "a"), Ok(()));
  }

  // A zero TTL is expired the moment it is granted (deadline == grant
  // instant, and the monotonic clock never runs backwards), so expiry is
  // tested without sleeping.
  #[test]
  fn expired_lease_has_no_holder_and_can_be_taken_over() {
    let lease = MemoryLease::new();
    lease.try_acquire("job", "a", Duration::ZERO).expect("a");
    assert_eq!(lease.holder("job"), None);
    lease.try_acquire("job", "b", LONG).expect("b takes over");
    assert_eq!(lease.holder("job").as_deref(), Some("b"));
  }

  #[test]
  fn expired_lease_cannot_be_renewed() {
    let lease = MemoryLease::new();
    lease.try_acquire("job", "a", Duration::ZERO).expect("a");
    assert!(matches!(
      lease.renew("job", "a", LONG),
      Err(SingletonError::Unavailable(_))
    ));
  }

  #[test]
  fn renew_extends_a_lease_that_would_have_expired() {
    let lease = MemoryLease::new();
    lease.try_acquire("job", "a", LONG).expect("a");
    lease.renew("job", "a", Duration::ZERO).expect("shrink");
    assert_eq!(lease.holder("job"), None, "renewal sets a new deadline");
  }

  #[test]
  fn singleton_tracks_its_local_pid() {
    let lease: Arc<dyn SingletonLease> = Arc::new(MemoryLease::new());
    let single = ClusterSingleton::new("leader", "a", lease, LONG);
    assert_eq!(single.local_pid(), None);
    assert!(!single.is_holder());
    single.try_become(&pid()).expect("become");
    assert_eq!(single.local_pid(), Some(pid()));
    single.heartbeat().expect("heartbeat");
    single.resign().expect("resign");
    assert_eq!(single.local_pid(), None);
    assert!(!single.is_holder());
  }

  #[test]
  fn failed_takeover_records_no_pid() {
    let lease = Arc::new(MemoryLease::new());
    let a = ClusterSingleton::new("leader", "a", Arc::clone(&lease) as _, LONG);
    let b = ClusterSingleton::new("leader", "b", lease as _, LONG);
    a.try_become(&pid()).expect("a");
    assert!(b.try_become(&pid()).is_err());
    assert_eq!(b.local_pid(), None);
    assert!(matches!(b.heartbeat(), Err(SingletonError::HeldBy(h)) if h == "a"));
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
    match PgAdvisoryLease::connect("not-a-postgres-url") {
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
          held: Mutex::new(HashMap::new()),
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
        held: Mutex::new(HashMap::from([("job".to_string(), "a".to_string())])),
      }
    }

    #[test]
    fn acquire_is_reentrant_and_exclusive_per_process() {
      let f = fake(Script::default());
      f.lease.try_acquire("job", "a", LONG).expect("acquire");
      f.lease.try_acquire("job", "a", LONG).expect("re-acquire");
      assert_eq!(
        f.lease.try_acquire("job", "b", LONG),
        Err(SingletonError::HeldBy("a".into()))
      );
      assert_eq!(f.lease.holder("job").as_deref(), Some("a"));
      assert_eq!(f.lease.holder("other"), None);
    }

    #[test]
    fn lock_held_by_another_session_is_refused() {
      let f = fake(Script {
        contested: vec![advisory_lock_keys("job")],
        ..Script::default()
      });
      assert_eq!(
        f.lease.try_acquire("job", "a", LONG),
        Err(SingletonError::HeldBy("another session".into()))
      );
      assert_eq!(f.lease.holder("job"), None);
      f.lease.try_acquire("free", "a", LONG).expect("other key");
    }

    #[test]
    fn acquire_sql_error_is_unavailable() {
      let f = fake(Script {
        acquire_err: Some("permission denied".into()),
        ..Script::default()
      });
      assert_eq!(
        f.lease.try_acquire("job", "a", LONG),
        Err(SingletonError::Unavailable("permission denied".into()))
      );
    }

    #[test]
    fn renew_tracks_session_liveness() {
      let f = fake(Script::default());
      assert_eq!(
        f.lease.renew("job", "a", LONG),
        Err(SingletonError::Unavailable("no lease".into()))
      );
      f.lease.try_acquire("job", "a", LONG).expect("acquire");
      f.lease.renew("job", "a", LONG).expect("renew");
      assert_eq!(
        f.lease.renew("job", "b", LONG),
        Err(SingletonError::HeldBy("a".into()))
      );
      f.session_alive.store(false, Ordering::SeqCst);
      assert_eq!(
        f.lease.renew("job", "a", LONG),
        Err(SingletonError::Unavailable("connection reset".into()))
      );
      assert_eq!(f.lease.holder("job"), None, "a dead session holds nothing");
    }

    #[test]
    fn release_unlocks_only_for_the_holder() {
      let f = fake(Script::default());
      assert_eq!(f.lease.release("job", "a"), Ok(()));
      f.lease.try_acquire("job", "a", LONG).expect("acquire");
      assert_eq!(
        f.lease.release("job", "b"),
        Err(SingletonError::HeldBy("a".into()))
      );
      assert!(f.unlocked.lock().unwrap().is_empty());
      f.lease.release("job", "a").expect("release");
      assert_eq!(*f.unlocked.lock().unwrap(), vec![advisory_lock_keys("job")]);
      f.lease
        .try_acquire("job", "b", LONG)
        .expect("b after release");
    }

    #[test]
    fn missing_worker_is_unavailable() {
      let lease = worker_gone();
      let gone = SingletonError::Unavailable("lease worker gone".into());
      assert_eq!(lease.try_acquire("other", "a", LONG), Err(gone.clone()));
      assert_eq!(lease.renew("job", "a", LONG), Err(gone));
      assert_eq!(lease.holder("job"), None);
    }

    #[test]
    fn dropped_response_is_unavailable() {
      let f = fake(Script {
        drop_responses: true,
        ..Script::default()
      });
      let dropped = SingletonError::Unavailable("lease worker dropped response".into());
      assert_eq!(f.lease.try_acquire("job", "a", LONG), Err(dropped.clone()));
      f.lease
        .held
        .lock()
        .unwrap()
        .insert("job".into(), "a".into());
      assert_eq!(f.lease.renew("job", "a", LONG), Err(dropped));
      // Release still clears local state even if the unlock reply is lost.
      assert_eq!(f.lease.release("job", "a"), Ok(()));
      assert!(f.lease.held.lock().unwrap().is_empty());
    }
  }

  #[cfg(feature = "pg")]
  #[test]
  fn pg_lease_from_pool_config_uses_its_url() {
    let config = id_effect_sql::pg::PgPoolConfig::from_url("not-a-postgres-url");
    assert!(matches!(
      PgAdvisoryLease::from_pool_config(config),
      Err(SingletonError::Unavailable(_))
    ));
  }
}
