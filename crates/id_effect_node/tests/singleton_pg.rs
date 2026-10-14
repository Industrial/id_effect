//! Integration tests for the Postgres advisory-lock `SingletonLease` (feature `pg`).
//!
//! Live tests need a database at `DATABASE_URL` (or the older `IDE_PG_TEST_URL`). When CI
//! provides a Postgres service it also sets `ID_EFFECT_REQUIRE_PG=1`, which turns a missing or
//! unreachable database into a test **failure**. Without it, live tests print an explicit `SKIP`
//! line to stderr and return (same convention as `id_effect_sql`'s `tests/pg.rs`).

#![cfg(feature = "pg")]

use std::time::Duration;

use id_effect_node::singleton::{PgAdvisoryLease, SingletonError, SingletonLease};

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
  match PgAdvisoryLease::connect(&url) {
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
  let err = PgAdvisoryLease::connect("postgres://id_effect@127.0.0.1:1/none")
    .err()
    .expect("connect must fail");
  assert!(matches!(err, SingletonError::Unavailable(_)), "{err}");
}

#[test]
fn pg_advisory_lease_enforces_single_holder() {
  let Some(url) = live_url("pg_advisory_lease_enforces_single_holder") else {
    return;
  };
  let ttl = Duration::from_secs(30);
  let name = lock_name("single");

  let node_a = PgAdvisoryLease::connect(&url).expect("node a connects");
  let node_b = PgAdvisoryLease::connect(&url).expect("node b connects");

  // First contender wins the advisory lock.
  node_a.try_acquire(&name, "a", ttl).expect("a acquires");
  assert_eq!(node_a.holder(&name).as_deref(), Some("a"));

  // Second contender on a separate session is refused.
  assert!(matches!(
    node_b.try_acquire(&name, "b", ttl),
    Err(SingletonError::HeldBy(_))
  ));

  // Heartbeat succeeds while the session is alive, then the holder steps down.
  node_a.renew(&name, "a", ttl).expect("a renews");
  node_a.release(&name, "a").expect("a releases");

  // After release the lock is available to the other node.
  node_b
    .try_acquire(&name, "b", ttl)
    .expect("b acquires after release");
  assert_eq!(node_b.holder(&name).as_deref(), Some("b"));
  node_b.release(&name, "b").expect("b releases");
}

#[test]
fn dropping_the_holder_session_fails_over() {
  let Some(url) = live_url("dropping_the_holder_session_fails_over") else {
    return;
  };
  let ttl = Duration::from_secs(30);
  let name = lock_name("failover");

  let node_a = PgAdvisoryLease::connect(&url).expect("node a connects");
  let node_b = PgAdvisoryLease::connect(&url).expect("node b connects");
  node_a.try_acquire(&name, "a", ttl).expect("a acquires");
  assert!(node_b.try_acquire(&name, "b", ttl).is_err());

  // The lock is session-scoped: when a's connection goes away, so does the lock. Postgres
  // processes the disconnect asynchronously, so retry until it lands (bounded).
  drop(node_a);
  let deadline = std::time::Instant::now() + Duration::from_secs(10);
  loop {
    match node_b.try_acquire(&name, "b", ttl) {
      Ok(()) => break,
      Err(e) if std::time::Instant::now() >= deadline => panic!("no failover: {e}"),
      Err(_) => std::thread::yield_now(),
    }
  }
  assert_eq!(node_b.holder(&name).as_deref(), Some("b"));
}

#[test]
fn renew_and_release_respect_ownership() {
  let Some(url) = live_url("renew_and_release_respect_ownership") else {
    return;
  };
  let ttl = Duration::from_secs(30);
  let name = lock_name("ownership");
  let lease = PgAdvisoryLease::connect(&url).expect("connects");

  assert!(matches!(
    lease.renew(&name, "a", ttl),
    Err(SingletonError::Unavailable(_))
  ));
  lease.try_acquire(&name, "a", ttl).expect("a acquires");
  lease
    .try_acquire(&name, "a", ttl)
    .expect("re-entrant for a");
  assert!(matches!(
    lease.renew(&name, "b", ttl),
    Err(SingletonError::HeldBy(h)) if h == "a"
  ));
  assert!(matches!(
    lease.release(&name, "b"),
    Err(SingletonError::HeldBy(_))
  ));
  lease.release(&name, "a").expect("a releases");
  assert_eq!(lease.holder(&name), None);
}
