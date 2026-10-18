//! Integration tests for the Postgres advisory-lock `SingletonLease` (feature `pg`).
//!
//! Live tests need a database at `DATABASE_URL` (or the older `IDE_PG_TEST_URL`). When CI
//! provides a Postgres service it also sets `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or
//! unreachable database into a test **failure**. Without it, live tests print an explicit `SKIP`
//! line to stderr and return (same convention as `id_effect_sql`'s `tests/pg.rs`).

#![cfg(feature = "pg")]

use std::sync::Arc;
use std::time::Duration;

use id_effect::{Cause, Exit, Never};
use id_effect::{Effect, Runtime, TokioExecutor};
use id_effect_node::singleton::{
  PgAdvisoryLease, SingletonError, SingletonLease, advisory_lock_keys,
};
use id_effect_sql::pg::{PgPoolConfig, PgTlsMode};

/// Run a lease effect on the default runtime (its tokio side has the IO driver sqlx needs; the
/// session fiber the connect forks lands there).
fn run<A: Send + 'static>(eff: Effect<A, SingletonError>) -> Result<A, SingletonError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn get<A: Send + 'static>(eff: Effect<A, Never>) -> A {
  match eff.block_on() {
    Exit::Success(a) => a,
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

/// On an executor without tokio's IO driver the session fiber dies before dialing; connect
/// reports that defect instead of hanging (no database needed).
#[test]
fn connect_without_an_io_driver_is_unavailable() {
  let timer_only = tokio::runtime::Builder::new_multi_thread()
    .worker_threads(1)
    .enable_time()
    .build()
    .expect("tokio runtime");
  let rt = Runtime::from_executor(Arc::new(TokioExecutor::new(timer_only.handle().clone())));
  let err = match rt.block_on(PgAdvisoryLease::connect(
    "postgres://id_effect@127.0.0.1:1/none",
  )) {
    Exit::Failure(Cause::Fail(e)) => e,
    Exit::Success(_) => panic!("connect must fail without an IO driver"),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  };
  assert!(
    matches!(&err, SingletonError::Unavailable(m) if m.starts_with("lease worker exited before ready")),
    "{err}"
  );
}

fn pg_required() -> bool {
  std::env::var("ID_EFFECT_REQUIRE_PG").is_ok_and(|v| v == "1")
}

/// Fail (when `ID_EFFECT_REQUIRE_PG=1`) or loudly skip a live test that has no database.
fn unavailable(test: &str, why: &str) -> Option<String> {
  assert!(
    !pg_required(),
    "{test}: ID_EFFECT_REQUIRE_PG=1 but Postgres is unavailable: {why}"
  );
  eprintln!("SKIP {test}: {why} (set DATABASE_URL, and ID_EFFECT_REQUIRE_PG=1 to require it)");
  None
}

/// Database URL for a live test, or `None` after [`unavailable`] decided to skip.
fn live_url(test: &str) -> Option<String> {
  let url = ["DATABASE_URL", "IDE_PG_TEST_URL"]
    .into_iter()
    .find_map(|key| std::env::var(key).ok().filter(|url| !url.is_empty()));
  let Some(url) = url else {
    return unavailable(test, "DATABASE_URL is not set");
  };
  match run(PgAdvisoryLease::connect(&url)) {
    Ok(_) => Some(url),
    Err(e) => unavailable(test, &format!("cannot connect: {e}")),
  }
}

fn lock_name(test: &str) -> String {
  format!("id_effect-it-{test}-{}", std::process::id())
}

#[test]
fn unreachable_database_is_unavailable() {
  // Port 1 on loopback refuses immediately; no database needed.
  let err = run(PgAdvisoryLease::connect(
    "postgres://id_effect@127.0.0.1:1/none",
  ))
  .err()
  .expect("connect must fail");
  assert!(matches!(err, SingletonError::Unavailable(_)), "{err}");
}

#[test]
fn advisory_lease_uses_pool_tls() {
  let Some(url) = live_url("advisory_lease_uses_pool_tls") else {
    return;
  };
  // The config's explicit TLS settings apply, not only the URL's: a certificate no server has
  // fails (a plaintext server already refuses the TLS `VerifyFull` needs) ...
  let verified = PgPoolConfig::from_url(&url)
    .with_tls_mode(PgTlsMode::VerifyFull)
    .with_root_cert_pem(b"not a certificate".to_vec());
  match run(PgAdvisoryLease::from_pool_config(verified)) {
    Err(SingletonError::Unavailable(m)) => assert!(m.starts_with("connect:"), "{m}"),
    Err(other) => panic!("unexpected error: {other}"),
    Ok(_) => panic!("VerifyFull with a bogus root certificate must not connect"),
  }
  // ... and an explicit `Disable` overrides the URL's `sslmode=verify-full`.
  let sep = if url.contains('?') { '&' } else { '?' };
  let plaintext = PgPoolConfig::from_url(format!("{url}{sep}sslmode=verify-full"))
    .with_tls_mode(PgTlsMode::Disable);
  let lease = run(PgAdvisoryLease::from_pool_config(plaintext)).expect("Disable connects");
  let name = lock_name("pool-tls");
  run(lease.try_acquire(&name, "a", Duration::from_secs(30))).expect("acquires");
  run(lease.release(&name, "a")).expect("releases");
}

#[test]
fn pg_advisory_lease_enforces_single_holder() {
  let Some(url) = live_url("pg_advisory_lease_enforces_single_holder") else {
    return;
  };
  let ttl = Duration::from_secs(30);
  let name = lock_name("single");

  let node_a = run(PgAdvisoryLease::connect(&url)).expect("node a connects");
  let node_b = run(PgAdvisoryLease::connect(&url)).expect("node b connects");

  // First contender wins the advisory lock.
  run(node_a.try_acquire(&name, "a", ttl)).expect("a acquires");
  assert_eq!(get(node_a.holder(&name)).as_deref(), Some("a"));

  // Second contender on a separate session is refused.
  assert!(matches!(
    run(node_b.try_acquire(&name, "b", ttl)),
    Err(SingletonError::HeldBy(_))
  ));

  // Heartbeat succeeds while the session is alive, then the holder steps down.
  run(node_a.renew(&name, "a", ttl)).expect("a renews");
  run(node_a.release(&name, "a")).expect("a releases");

  // After release the lock is available to the other node.
  run(node_b.try_acquire(&name, "b", ttl)).expect("b acquires after release");
  assert_eq!(get(node_b.holder(&name)).as_deref(), Some("b"));
  run(node_b.release(&name, "b")).expect("b releases");
}

#[test]
fn dropping_the_holder_session_fails_over() {
  let Some(url) = live_url("dropping_the_holder_session_fails_over") else {
    return;
  };
  let ttl = Duration::from_secs(30);
  let name = lock_name("failover");

  let node_a = run(PgAdvisoryLease::connect(&url)).expect("node a connects");
  let node_b = run(PgAdvisoryLease::connect(&url)).expect("node b connects");
  run(node_a.try_acquire(&name, "a", ttl)).expect("a acquires");
  assert!(run(node_b.try_acquire(&name, "b", ttl)).is_err());

  // The lock is session-scoped: dropping the handle interrupts its session fiber, the
  // connection goes away, and so does the lock. Postgres processes the disconnect
  // asynchronously, so retry until it lands (bounded).
  drop(node_a);
  let deadline = std::time::Instant::now() + Duration::from_secs(10);
  loop {
    match run(node_b.try_acquire(&name, "b", ttl)) {
      Ok(()) => break,
      Err(e) if std::time::Instant::now() >= deadline => panic!("no failover: {e}"),
      Err(_) => std::thread::sleep(Duration::from_millis(10)),
    }
  }
  assert_eq!(get(node_b.holder(&name)).as_deref(), Some("b"));
}

/// Terminate (from a separate connection) the backend holding the advisory lock for `name`.
fn terminate_lock_holder(url: &str, name: &str) -> i64 {
  use sqlx::Connection as _;
  let (k1, k2) = advisory_lock_keys(name);
  let rt = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .expect("tokio runtime");
  rt.block_on(async {
    let mut admin = sqlx::postgres::PgConnection::connect(url).await?;
    // Two-key advisory locks: classid = key1, objid = key2, objsubid = 2.
    sqlx::query_scalar::<_, i64>(
      "SELECT count(*) FROM (SELECT pg_terminate_backend(pid) FROM pg_locks \
       WHERE locktype = 'advisory' AND granted AND objsubid = 2 \
       AND classid = $1::int4::oid AND objid = $2::int4::oid) t",
    )
    .bind(k1)
    .bind(k2)
    .fetch_one(&mut admin)
    .await
  })
  .expect("terminate the lock holder")
}

#[test]
fn terminated_session_does_not_reacquire() {
  let Some(url) = live_url("terminated_session_does_not_reacquire") else {
    return;
  };
  let ttl = Duration::from_secs(30);
  let name = lock_name("terminated");
  let node_a = run(PgAdvisoryLease::connect(&url)).expect("node a connects");
  let node_b = run(PgAdvisoryLease::connect(&url)).expect("node b connects");
  run(node_a.try_acquire(&name, "a", ttl)).expect("a acquires");
  assert_eq!(terminate_lock_holder(&url, &name), 1, "one session held it");

  // a's session is gone and so is its lock: a must not "re-acquire" from its own bookkeeping.
  assert!(matches!(
    run(node_a.try_acquire(&name, "a", ttl)),
    Err(SingletonError::Unavailable(_))
  ));
  assert_eq!(get(node_a.holder(&name)), None);

  // The lock really is free: b takes it (Postgres frees it asynchronously, so retry, bounded).
  let deadline = std::time::Instant::now() + Duration::from_secs(10);
  loop {
    match run(node_b.try_acquire(&name, "b", ttl)) {
      Ok(()) => break,
      Err(e) if std::time::Instant::now() >= deadline => panic!("lock not freed: {e}"),
      Err(_) => std::thread::sleep(Duration::from_millis(10)),
    }
  }
  run(node_b.release(&name, "b")).expect("b releases");
}

#[test]
fn renew_and_release_respect_ownership() {
  let Some(url) = live_url("renew_and_release_respect_ownership") else {
    return;
  };
  let ttl = Duration::from_secs(30);
  let name = lock_name("ownership");
  let lease = run(PgAdvisoryLease::connect(&url)).expect("connects");

  assert!(matches!(
    run(lease.renew(&name, "a", ttl)),
    Err(SingletonError::Unavailable(_))
  ));
  run(lease.try_acquire(&name, "a", ttl)).expect("a acquires");
  run(lease.try_acquire(&name, "a", ttl)).expect("re-entrant for a");
  assert!(matches!(
    run(lease.renew(&name, "b", ttl)),
    Err(SingletonError::HeldBy(h)) if h == "a"
  ));
  assert!(matches!(
    run(lease.release(&name, "b")),
    Err(SingletonError::HeldBy(_))
  ));
  run(lease.release(&name, "a")).expect("a releases");
  assert_eq!(get(lease.holder(&name)), None);
}
