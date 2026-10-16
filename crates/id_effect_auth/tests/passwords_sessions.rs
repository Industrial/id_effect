//! Passwords and sessions through the public API: the capability environment, rehash on login,
//! session fixation, revoke-all, and expiry on an anchored clock.

use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Exit, HCons, HNil, Layer, UtcDateTime, WallClock, hlist};
use id_effect_auth::password::{hash_password, verify_password};
use id_effect_auth::session::{load_session, login, revoke_all};
use id_effect_auth::{
  MemorySessionStore, PasswordError, PasswordHasher, PasswordParams, Session, SessionConfig,
  SessionError, Sessions, Verification,
};

/// Low-cost argon2id so the suite stays fast; production uses `PasswordHasher::default()`.
fn fast_hasher(m_cost_kib: u32) -> PasswordHasher {
  PasswordHasher::new(PasswordParams::new(m_cost_kib, 1, 1).expect("valid params"))
}

fn ok<A, E: std::fmt::Debug>(exit: Exit<A, E>) -> A {
  match exit {
    Exit::Success(a) => a,
    Exit::Failure(Cause::Fail(e)) => panic!("failed: {e:?}"),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

#[test]
fn default_hasher_uses_owasp_argon2id_parameters() {
  let params = PasswordHasher::default().params();
  assert_eq!(params, PasswordParams::OWASP);
  assert_eq!(
    (params.m_cost_kib(), params.t_cost(), params.p_cost()),
    (19 * 1024, 2, 1)
  );
}

#[test]
fn password_capability_hashes_and_verifies_in_the_environment() {
  type Env = HCons<PasswordHasher, HNil>;
  let program: Effect<(Verification, Verification, Verification), PasswordError, Env> =
    hash_password::<Env, _>("hunter2").flat_map(|stored| {
      let again = stored.clone();
      verify_password::<Env, _>("hunter2", Some(stored)).flat_map(move |right| {
        verify_password::<Env, _>("hunter3", Some(again)).flat_map(move |wrong| {
          verify_password::<Env, _>("hunter2", None).map(move |unknown| (right, wrong, unknown))
        })
      })
    });
  let (right, wrong, unknown) = ok(
    program
      .provide_layer(Layer::succeed(hlist![fast_hasher(8)]))
      .block_on(),
  );
  assert_eq!(right, Verification::Valid);
  assert_eq!(wrong, Verification::Invalid);
  assert_eq!(unknown, Verification::Invalid);
}

#[test]
fn raising_the_cost_upgrades_hashes_on_the_next_login() {
  let stored = ok(fast_hasher(8).hash("pw").block_on());
  let stronger = fast_hasher(32);
  let Verification::ValidNeedsRehash(upgraded) = ok(stronger.verify("pw", &stored).block_on())
  else {
    panic!("expected an upgraded hash")
  };
  assert_eq!(
    ok(stronger.verify("pw", &upgraded).block_on()),
    Verification::Valid
  );
}

type SessEnv = HCons<Sessions, HNil>;

fn sessions_on(clock: &WallClock) -> (Sessions, MemorySessionStore) {
  let store = MemorySessionStore::new();
  let config = SessionConfig {
    idle_timeout: Duration::from_secs(600),
    absolute_timeout: Duration::from_secs(3600),
    ..SessionConfig::default()
  };
  let sessions = Sessions::new(store.clone(), config).with_clock(clock.clone());
  (sessions, store)
}

fn anchored() -> WallClock {
  WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_800_000_000_000))
}

#[test]
fn login_never_reuses_a_presented_id() {
  let clock = anchored();
  let (sessions, store) = sessions_on(&clock);
  let program: Effect<(Session, Session, Option<Session>), SessionError, SessEnv> =
    login::<SessEnv, _>("guest", None).flat_map(|planted| {
      let presented = planted.id().clone();
      login::<SessEnv, _>("alice", Some(presented.clone())).flat_map(move |fresh| {
        load_session::<SessEnv, _>(presented).map(move |stale| (planted, fresh, stale))
      })
    });
  let (planted, fresh, stale) = ok(program.provide_all(hlist![sessions]).block_on());
  assert_ne!(planted.id(), fresh.id());
  assert_eq!(stale, None, "the pre-login id no longer resolves");
  assert_eq!(store.len(), 1);
}

#[test]
fn revoke_all_ends_every_session_of_a_user() {
  let clock = anchored();
  let (sessions, store) = sessions_on(&clock);
  let program: Effect<u64, SessionError, SessEnv> = login::<SessEnv, _>("alice", None)
    .flat_map(|_| login::<SessEnv, _>("alice", None))
    .flat_map(|_| login::<SessEnv, _>("bob", None))
    .flat_map(|_| revoke_all::<SessEnv, _>("alice"));
  assert_eq!(ok(program.provide_all(hlist![sessions]).block_on()), 2);
  assert_eq!(store.len(), 1);
}

#[test]
fn sessions_expire_on_idle_and_absolute_timeouts() {
  let clock = anchored();
  let (sessions, _) = sessions_on(&clock);
  let idle = ok(sessions.login("alice", None).block_on());
  let busy = ok(sessions.login("bob", None).block_on());
  // Bob uses his session every 5 minutes; Alice does not.
  for _ in 0..2 {
    clock.jump(Duration::from_secs(300));
    assert!(ok(sessions.load(busy.id()).block_on()).is_some());
  }
  assert_eq!(
    ok(sessions.load(idle.id()).block_on()),
    None,
    "idle timeout"
  );
  for _ in 0..10 {
    clock.jump(Duration::from_secs(300));
    let _ = ok(sessions.load(busy.id()).block_on());
  }
  assert_eq!(
    ok(sessions.load(busy.id()).block_on()),
    None,
    "absolute timeout after 60 minutes of use"
  );
}

#[test]
fn the_session_cookie_is_hardened_by_default() {
  let clock = anchored();
  let (sessions, _) = sessions_on(&clock);
  let s = ok(sessions.login("alice", None).block_on());
  let header = sessions.set_cookie(&s);
  for attr in [
    "__Host-id_session=",
    "Path=/",
    "Secure",
    "HttpOnly",
    "SameSite=Lax",
  ] {
    assert!(header.contains(attr), "{attr} missing from {header}");
  }
  assert!(!header.contains("Domain="));
  let request_cookie = format!("a=b; {}", &header[..header.find(';').unwrap()]);
  assert_eq!(sessions.session_id(&request_cookie).as_ref(), Some(s.id()));
}
