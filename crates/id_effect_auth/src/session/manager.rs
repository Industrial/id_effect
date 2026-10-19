//! [`Sessions`]: the session capability (login, load, rotate, logout, revoke-all) over a
//! [`SessionStore`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{
  Effect, HCons, HList, HNil, Layer, Selector, UtcDateTime, WallClock, fail, hlist, need, succeed,
};

use super::store::{SessionStore, SessionStoreEnv, SessionStoreService};
use super::{Session, SessionConfig, SessionError, SessionId};

/// `t + d`, saturating at the end of time.
fn plus(t: UtcDateTime, d: Duration) -> UtcDateTime {
  UtcDateTime(t.0.checked_add(d).unwrap_or(jiff::Timestamp::MAX))
}

/// Capability: server-side sessions over a [`SessionStore`]. Cheap to clone.
#[derive(Clone)]
pub struct Sessions {
  store: Arc<dyn SessionStore>,
  config: Arc<SessionConfig>,
  clock: WallClock,
}

impl fmt::Debug for Sessions {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Sessions")
      .field("config", &self.config)
      .field("clock", &self.clock)
      .finish_non_exhaustive()
  }
}

impl Sessions {
  /// Sessions in `store`, on the system clock.
  pub fn new(store: impl SessionStore, config: SessionConfig) -> Self {
    Self::from_service(SessionStoreService::new(store), config)
  }

  /// Sessions in the store capability `store`, on the system clock.
  pub fn from_service(store: SessionStoreService, config: SessionConfig) -> Self {
    Self {
      store: store.0,
      config: Arc::new(config),
      clock: WallClock::system(),
    }
  }

  /// The same sessions read time from `clock` (an anchored clock follows a `TestExecutor`'s
  /// virtual time).
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// Lifetimes and cookie.
  #[inline]
  pub fn config(&self) -> &SessionConfig {
    &self.config
  }

  fn now(&self) -> Effect<UtcDateTime, SessionError> {
    self.clock.now()
  }

  /// Start a session for `user_id` with a fresh id. `presented` is the id the client sent with
  /// the login request, if any: it is removed, so an id planted before login (session
  /// fixation) never becomes an authenticated session. Send the result's
  /// [`set_cookie`](Self::set_cookie). Behind a CSRF layer, use `login_with_csrf` (feature
  /// `axum`), which also rotates the CSRF token.
  pub fn login(
    &self,
    user_id: impl Into<String>,
    presented: Option<&SessionId>,
  ) -> Effect<Session, SessionError> {
    let (this, user_id) = (self.clone(), user_id.into());
    self.drop_presented(presented).flat_map(move |()| {
      this.now().flat_map(move |now| {
        let id = match SessionId::generate() {
          Ok(id) => id,
          Err(e) => return fail(e),
        };
        let absolute = plus(now, this.config.absolute_timeout);
        let idle = plus(now, this.config.idle_timeout).min(absolute);
        let session = Session::from_parts(id, user_id, BTreeMap::new(), now, now, idle, absolute);
        this.store.save(session.clone()).map(move |()| session)
      })
    })
  }

  /// [`login`](Self::login) for a browser behind `id_effect_axum`'s `SecurityLayer`: once the
  /// session is saved under its fresh id, `csrf` (the request's
  /// [`CsrfToken`](id_effect_axum::server::security::CsrfToken)) is rotated, so no CSRF token
  /// rendered before login validates after it (L5). Return the new token from the handler (or
  /// render it) along with the result's [`set_cookie`](Self::set_cookie); the layer sets the new
  /// CSRF cookie. A failed login rotates nothing.
  #[cfg(feature = "axum")]
  pub fn login_with_csrf(
    &self,
    user_id: impl Into<String>,
    presented: Option<&SessionId>,
    csrf: &id_effect_axum::server::security::CsrfToken,
  ) -> Effect<(Session, id_effect_axum::server::security::CsrfToken), SessionError> {
    let csrf = csrf.clone();
    self
      .login(user_id, presented)
      .map(move |session| (session, csrf.rotate()))
  }

  fn drop_presented(&self, presented: Option<&SessionId>) -> Effect<(), SessionError> {
    match presented {
      Some(id) => self.store.remove(id).map(|_| ()),
      None => succeed(()),
    }
  }

  /// The live session with id `id`. An expired session (idle or absolute timeout) is removed
  /// and reads as `None`. A live one has its idle expiry pushed forward, written back at most
  /// once per [`SessionConfig::touch_interval`].
  pub fn load(&self, id: &SessionId) -> Effect<Option<Session>, SessionError> {
    let this = self.clone();
    self.store.load(id).flat_map(move |found| match found {
      None => succeed(None),
      Some(session) => this.now().flat_map(move |now| this.refresh(session, now)),
    })
  }

  fn refresh(
    &self,
    mut session: Session,
    now: UtcDateTime,
  ) -> Effect<Option<Session>, SessionError> {
    if session.is_expired(now) {
      return self.store.remove(&session.id).map(|_| None);
    }
    if now < plus(session.last_seen_at, self.config.touch_interval) {
      return succeed(Some(session));
    }
    session.last_seen_at = now;
    session.idle_expires_at = plus(now, self.config.idle_timeout).min(session.absolute_expires_at);
    self
      .store
      .save(session.clone())
      .map(move |()| Some(session))
  }

  /// Persist `session` (after [`Session::set_attr`] / [`Session::remove_attr`]).
  pub fn save(&self, session: Session) -> Effect<(), SessionError> {
    self.store.save(session)
  }

  /// Give the live session `id` a fresh id, keeping its user, attributes and expiries (do it
  /// after a privilege change such as a role grant or a second factor). `None` when `id` is not
  /// a live session. Send the result's [`set_cookie`](Self::set_cookie).
  pub fn rotate(&self, id: &SessionId) -> Effect<Option<Session>, SessionError> {
    let this = self.clone();
    self.load(id).flat_map(move |found| {
      let Some(old) = found else {
        return succeed(None);
      };
      let id = match SessionId::generate() {
        Ok(id) => id,
        Err(e) => return fail(e),
      };
      let fresh = Session { id, ..old.clone() };
      let store = Arc::clone(&this.store);
      this
        .store
        .save(fresh.clone())
        .flat_map(move |()| store.remove(&old.id))
        .map(move |_| Some(fresh))
    })
  }

  /// End the session `id`; `true` if it existed. Send [`clear_cookie`](Self::clear_cookie).
  pub fn logout(&self, id: &SessionId) -> Effect<bool, SessionError> {
    self.store.remove(id)
  }

  /// End every session of `user_id` (password change, "log out everywhere", account lock);
  /// how many ended.
  pub fn revoke_all(&self, user_id: &str) -> Effect<u64, SessionError> {
    self.store.remove_user(user_id)
  }

  /// Remove every expired record from the store; how many. Schedule it (for example with
  /// `Effect::repeat` over `Schedule::spaced`) so abandoned sessions do not accumulate.
  pub fn purge_expired(&self) -> Effect<u64, SessionError> {
    let store = Arc::clone(&self.store);
    self.now().flat_map(move |now| store.purge_expired(now))
  }

  /// The `Set-Cookie` value for `session`, alive until its absolute expiry.
  pub fn set_cookie(&self, session: &Session) -> String {
    let max_age = session
      .last_seen_at
      .distance_duration(&session.absolute_expires_at);
    self.config.cookie.set_cookie(&session.id, max_age)
  }

  /// The `Set-Cookie` value that deletes the session cookie.
  pub fn clear_cookie(&self) -> String {
    self.config.cookie.clear_cookie()
  }

  /// The session id in a `Cookie` request header value, if present and well formed.
  pub fn session_id(&self, cookie_header: &str) -> Option<SessionId> {
    self.config.cookie.find(cookie_header)
  }
}

/// The minimal typed environment holding only [`Sessions`].
pub type SessionsEnv = HCons<Sessions, HNil>;

/// A [`Layer`] building [`Sessions`] with `config` over the [`SessionStoreService`] it needs
/// (feed it [`provide_session_store`](super::provide_session_store) with `provide_to`).
pub fn sessions_layer<E: Send + 'static>(
  config: SessionConfig,
) -> Layer<SessionsEnv, E, SessionStoreEnv> {
  Layer::effect(move || {
    let config = config.clone();
    need::<SessionStoreService>()
      .map(move |store| hlist![Sessions::from_service((*store).clone(), config)])
  })
}

/// [`Sessions::login`] on the sessions in `R`.
pub fn login<R, I>(
  user_id: impl Into<String>,
  presented: Option<SessionId>,
) -> Effect<Session, SessionError, R>
where
  R: Selector<Sessions, I> + HList,
{
  let user_id = user_id.into();
  need::<Sessions>().flat_map(move |s| s.login(user_id, presented.as_ref()).narrow())
}

/// [`Sessions::load`] on the sessions in `R`.
pub fn load_session<R, I>(id: SessionId) -> Effect<Option<Session>, SessionError, R>
where
  R: Selector<Sessions, I> + HList,
{
  need::<Sessions>().flat_map(move |s| s.load(&id).narrow())
}

/// [`Sessions::logout`] on the sessions in `R`.
pub fn logout<R, I>(id: SessionId) -> Effect<bool, SessionError, R>
where
  R: Selector<Sessions, I> + HList,
{
  need::<Sessions>().flat_map(move |s| s.logout(&id).narrow())
}

/// [`Sessions::revoke_all`] on the sessions in `R`.
pub fn revoke_all<R, I>(user_id: impl Into<String>) -> Effect<u64, SessionError, R>
where
  R: Selector<Sessions, I> + HList,
{
  let user_id = user_id.into();
  need::<Sessions>().flat_map(move |s| s.revoke_all(&user_id).narrow())
}

#[cfg(test)]
mod tests {
  use std::time::Instant;

  use id_effect::{Cause, Exit};

  use super::super::{MemorySessionStore, provide_session_store};
  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, SessionError>) -> Result<A, SessionError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  const MIN: Duration = Duration::from_secs(60);

  /// Sessions with a 10 min idle / 60 min absolute timeout on a clock the test moves.
  fn fixture() -> (Sessions, MemorySessionStore, WallClock) {
    let store = MemorySessionStore::new();
    let clock = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_800_000_000_000));
    let config = SessionConfig {
      idle_timeout: 10 * MIN,
      absolute_timeout: 60 * MIN,
      touch_interval: MIN,
      ..SessionConfig::default()
    };
    let sessions = Sessions::new(store.clone(), config).with_clock(clock.clone());
    (sessions, store, clock)
  }

  #[test]
  fn login_creates_a_loadable_session() {
    let (sessions, store, _) = fixture();
    let s = run(sessions.login("u1", None)).unwrap();
    assert_eq!(s.user_id(), "u1");
    assert_eq!(s.idle_expires_at(), plus(s.created_at(), 10 * MIN));
    assert_eq!(s.absolute_expires_at(), plus(s.created_at(), 60 * MIN));
    assert_eq!(run(sessions.load(s.id())).unwrap(), Some(s));
    assert_eq!(store.len(), 1);
  }

  #[test]
  fn login_rotates_the_presented_session_id() {
    let (sessions, store, _) = fixture();
    let planted = run(sessions.login("anonymous", None)).unwrap();
    let fresh = run(sessions.login("u1", Some(planted.id()))).unwrap();
    assert_ne!(fresh.id(), planted.id());
    assert_eq!(run(sessions.load(planted.id())).unwrap(), None);
    assert_eq!(store.len(), 1);
  }

  #[test]
  fn idle_timeout_ends_an_unused_session() {
    let (sessions, store, clock) = fixture();
    let s = run(sessions.login("u1", None)).unwrap();
    clock.jump(10 * MIN);
    assert_eq!(run(sessions.load(s.id())).unwrap(), None);
    assert!(store.is_empty(), "expired record removed on load");
  }

  #[test]
  fn use_slides_the_idle_expiry_but_not_the_absolute_one() {
    let (sessions, _, clock) = fixture();
    let s = run(sessions.login("u1", None)).unwrap();
    // Within the touch interval: no write, same record.
    clock.jump(Duration::from_secs(30));
    assert_eq!(run(sessions.load(s.id())).unwrap().as_ref(), Some(&s));
    // Used every 9 minutes, it outlives the idle timeout ...
    for _ in 0..6 {
      clock.jump(9 * MIN);
      let live = run(sessions.load(s.id())).unwrap().expect("still live");
      assert!(live.idle_expires_at() > s.idle_expires_at());
      assert!(live.idle_expires_at() <= s.absolute_expires_at());
    }
    // ... but not the absolute one (54.5 min so far).
    clock.jump(6 * MIN);
    assert_eq!(run(sessions.load(s.id())).unwrap(), None);
  }

  #[test]
  fn rotate_keeps_the_data_under_a_new_id() {
    let (sessions, store, _) = fixture();
    let mut s = run(sessions.login("u1", None)).unwrap();
    s.set_attr("role", "admin");
    run(sessions.save(s.clone())).unwrap();
    let rotated = run(sessions.rotate(s.id())).unwrap().expect("live");
    assert_ne!(rotated.id(), s.id());
    assert_eq!(rotated.attr("role"), Some("admin"));
    assert_eq!(rotated.absolute_expires_at(), s.absolute_expires_at());
    assert_eq!(run(sessions.load(s.id())).unwrap(), None);
    assert_eq!(store.len(), 1);
    let gone = SessionId::generate().unwrap();
    assert_eq!(run(sessions.rotate(&gone)).unwrap(), None);
  }

  #[test]
  fn logout_and_revoke_all() {
    let (sessions, store, _) = fixture();
    let a = run(sessions.login("u1", None)).unwrap();
    run(sessions.login("u1", None)).unwrap();
    run(sessions.login("u2", None)).unwrap();
    assert!(run(sessions.logout(a.id())).unwrap());
    assert!(!run(sessions.logout(a.id())).unwrap());
    run(sessions.login("u1", None)).unwrap();
    assert_eq!(run(sessions.revoke_all("u1")).unwrap(), 2);
    assert_eq!(store.len(), 1);
  }

  #[test]
  fn purge_expired_uses_the_clock() {
    let (sessions, store, clock) = fixture();
    run(sessions.login("u1", None)).unwrap();
    assert_eq!(run(sessions.purge_expired()).unwrap(), 0);
    clock.jump(11 * MIN);
    assert_eq!(run(sessions.purge_expired()).unwrap(), 1);
    assert!(store.is_empty());
  }

  #[test]
  fn store_failures_surface_as_errors() {
    let (sessions, store, _) = fixture();
    let s = run(sessions.login("u1", None)).unwrap();
    store.fail_with(Some("down"));
    let down = SessionError::Storage("down".into());
    assert_eq!(run(sessions.login("u1", None)).unwrap_err(), down);
    assert_eq!(run(sessions.load(s.id())).unwrap_err(), down);
    assert_eq!(run(sessions.rotate(s.id())).unwrap_err(), down);
  }

  #[test]
  fn cookies_follow_the_config() {
    let (sessions, _, _) = fixture();
    let s = run(sessions.login("u1", None)).unwrap();
    let header = sessions.set_cookie(&s);
    assert!(header.contains("Max-Age=3600"), "{header}");
    assert!(header.contains("; Secure; HttpOnly; SameSite=Lax"));
    let cookie = format!("{}={}", sessions.config().cookie.name, s.id().as_str());
    assert_eq!(sessions.session_id(&cookie), Some(s.id().clone()));
    assert!(sessions.clear_cookie().contains("Max-Age=0"));
    assert!(format!("{sessions:?}").contains("Sessions"));
  }

  #[test]
  fn expiry_saturates_instead_of_overflowing() {
    let t = UtcDateTime::unsafe_make(0);
    assert_eq!(plus(t, Duration::MAX).0, jiff::Timestamp::MAX);
  }

  #[test]
  fn layer_and_helpers_run_over_the_capability() {
    let (sessions, _, _) = fixture();
    let config = sessions.config().clone();
    let store = MemorySessionStore::new();
    let program: Effect<(bool, u64), SessionError, SessionsEnv> =
      login::<SessionsEnv, _>("u1", None)
        .flat_map(|s| {
          load_session::<SessionsEnv, _>(s.id().clone()).flat_map(move |loaded| {
            assert_eq!(loaded.as_ref(), Some(&s));
            logout::<SessionsEnv, _>(s.id().clone())
          })
        })
        .flat_map(|ended| revoke_all::<SessionsEnv, _>("u1").map(move |n| (ended, n)));
    let layer =
      provide_session_store::<SessionError>(store.clone()).provide_to(sessions_layer(config));
    assert_eq!(run(program.provide_layer(layer)), Ok((true, 0)));
    assert!(store.is_empty());
  }
}
