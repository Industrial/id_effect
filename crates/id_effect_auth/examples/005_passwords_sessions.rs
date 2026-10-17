//! Ex 005 — register, log in, use and end a session: argon2id passwords and server-side sessions
//! as capabilities in one typed environment.
//!
//! Run: `cargo run -p id_effect_auth --example 005_passwords_sessions`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use id_effect::{Effect, Exit, HCons, HNil, Layer, fail, hlist, need, succeed};
use id_effect_auth::password::{hash_password, verify_password};
use id_effect_auth::session::{load_session, login, logout};
use id_effect_auth::{
  MemorySessionStore, PasswordError, PasswordHash, PasswordHasher, Session, SessionConfig,
  SessionError, SessionId, Sessions, Verification,
};

/// A toy user table: user name → stored PHC string.
#[derive(Clone, Default)]
struct Users(Arc<Mutex<HashMap<String, PasswordHash>>>);

impl Users {
  fn get(&self, name: &str) -> Option<PasswordHash> {
    self.0.lock().expect("users").get(name).cloned()
  }

  fn put(&self, name: &str, hash: PasswordHash) {
    self.0.lock().expect("users").insert(name.to_owned(), hash);
  }
}

#[derive(Clone, Debug)]
enum AppError {
  Password(PasswordError),
  Session(SessionError),
  BadCredentials,
}

impl std::fmt::Display for AppError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Password(e) => write!(f, "{e}"),
      Self::Session(e) => write!(f, "{e}"),
      Self::BadCredentials => f.write_str("bad credentials"),
    }
  }
}

impl From<PasswordError> for AppError {
  fn from(e: PasswordError) -> Self {
    Self::Password(e)
  }
}

impl From<SessionError> for AppError {
  fn from(e: SessionError) -> Self {
    Self::Session(e)
  }
}

type Env = HCons<Users, HCons<PasswordHasher, HCons<Sessions, HNil>>>;

fn register(name: &'static str, password: &'static str) -> Effect<(), AppError, Env> {
  hash_password::<Env, _>(password)
    .map_err(AppError::from)
    .flat_map(move |hash| need::<Users>().map(move |users| users.put(name, hash)))
}

/// Check the password (an unknown user costs the same as a wrong password), store an upgraded
/// hash if the parameters changed, then start a session with a fresh id.
fn log_in(
  name: &'static str,
  password: &'static str,
  presented: Option<SessionId>,
) -> Effect<Session, AppError, Env> {
  need::<Users>().flat_map(move |users| {
    verify_password::<Env, _>(password, users.get(name))
      .map_err(AppError::from)
      .flat_map(move |verdict| match verdict {
        Verification::Invalid => fail(AppError::BadCredentials),
        Verification::Valid => succeed(()),
        Verification::ValidNeedsRehash(upgraded) => {
          users.put(name, upgraded);
          succeed(())
        }
      })
      .flat_map(move |()| login::<Env, _>(name, presented).map_err(AppError::from))
  })
}

fn program() -> Effect<(), AppError, Env> {
  register("ada", "correct horse battery staple")
    .flat_map(|()| {
      log_in("mallory", "guess", None).map(|_| ()).catch(|e| {
        println!("unknown user: {e}");
        succeed(())
      })
    })
    .flat_map(|()| log_in("ada", "correct horse battery staple", None))
    .flat_map(|session| {
      println!("logged in: {}", session.user_id());
      let id = session.id().clone();
      load_session::<Env, _>(id.clone())
        .map_err(AppError::from)
        .flat_map(move |loaded| {
          println!("request sees: {:?}", loaded.map(|s| s.user_id().to_owned()));
          logout::<Env, _>(id).map_err(AppError::from)
        })
    })
    .map(|ended| println!("logged out: {ended}"))
}

fn main() {
  // `PasswordHasher::default()` is argon2id with the OWASP parameters.
  let env: Layer<Env, AppError> = Layer::succeed(hlist![
    Users::default(),
    PasswordHasher::default(),
    Sessions::new(MemorySessionStore::new(), SessionConfig::default()),
  ]);
  match program().provide_layer(env).block_on() {
    Exit::Success(()) => {}
    Exit::Failure(cause) => panic!("example failed: {}", cause.pretty()),
  }
}
