//! Rooms in PostgreSQL: who is a member of which room ([`Rooms`]), the join policy that lets
//! members only into a room's channel ([`MembersOnly`]), and the room history as an event log
//! ([`Journal`]: one stream per room in id_effect_events' `SqlEventJournal`).
//!
//! Every node shares the database, so a member added on one node may join on any other at once.

use std::sync::Arc;

use id_effect::{Ctx, DynEnv, Effect, fail, succeed};
use id_effect_kit::authz::{AuthzError, Decision};
use id_effect_kit::events::{EventStoreError, SqlEventJournal};
use id_effect_kit::rpc::channels::{JoinPolicy, JoinRequest};
use id_effect_kit::sql::{SqlClient, SqlError, params};
use serde::{Deserialize, Serialize};

use crate::api::room_of;

/// The tables of rooms, members and RPC idempotency keys (`CREATE … IF NOT EXISTS`, applied by
/// [`Rooms::migrate`]).
pub const SCHEMA: [&str; 4] = [
  "CREATE TABLE IF NOT EXISTS relay_rooms (\
     name TEXT PRIMARY KEY, \
     created_by TEXT NOT NULL, \
     created_at TIMESTAMPTZ NOT NULL DEFAULT now())",
  "CREATE TABLE IF NOT EXISTS relay_members (\
     room TEXT NOT NULL REFERENCES relay_rooms (name) ON DELETE CASCADE, \
     member TEXT NOT NULL, \
     PRIMARY KEY (room, member))",
  // `crate::idempotency`: one row per (caller, procedure, key); `body` is null while the first
  // call runs; `digest` is the SHA-256 (hex) of the procedure and its payload.
  "CREATE TABLE IF NOT EXISTS relay_idempotency (\
     scope TEXT NOT NULL, \
     tag TEXT NOT NULL, \
     key TEXT NOT NULL, \
     digest TEXT NOT NULL, \
     status BIGINT, \
     body TEXT, \
     created_at TIMESTAMPTZ NOT NULL DEFAULT now(), \
     PRIMARY KEY (scope, tag, key))",
  "CREATE INDEX IF NOT EXISTS relay_idempotency_created_at_idx \
     ON relay_idempotency (scope, created_at)",
];

const CREATE_ROOM: &str = "WITH created AS (\
     INSERT INTO relay_rooms (name, created_by) VALUES ($1, $2) \
     ON CONFLICT (name) DO NOTHING RETURNING name) \
   INSERT INTO relay_members (room, member) SELECT name, $2 FROM created";

const ADD_MEMBER: &str = "INSERT INTO relay_members (room, member) VALUES ($1, $2) \
   ON CONFLICT (room, member) DO NOTHING";

const IS_MEMBER: &str = "SELECT count(*) FROM relay_members WHERE room = $1 AND member = $2";

/// What the history stores per message (the room is the stream, the number its version).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Posted {
  /// Who posted it.
  pub from: String,
  /// The node it was posted on.
  pub node: String,
  /// The text.
  pub text: String,
}

/// The room history: one event stream per room ([`crate::api::topic`]), positions global.
pub type Journal = SqlEventJournal<Posted>;

/// Rooms and their members.
#[derive(Clone)]
pub struct Rooms {
  db: Arc<dyn SqlClient>,
}

impl std::fmt::Debug for Rooms {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Rooms").finish_non_exhaustive()
  }
}

impl Rooms {
  /// Rooms in `db`.
  pub fn new(db: Arc<dyn SqlClient>) -> Self {
    Self { db }
  }

  /// The database.
  pub fn db(&self) -> &Arc<dyn SqlClient> {
    &self.db
  }

  /// The room history in the same database.
  pub fn journal(&self) -> Journal {
    SqlEventJournal::new(Arc::clone(&self.db))
  }

  /// Creates the tables of [`SCHEMA`] and the event journal's. Run once before the nodes start
  /// (concurrent `CREATE TABLE IF NOT EXISTS` may race in PostgreSQL).
  ///
  /// # Errors
  ///
  /// The failed statement's error, as text.
  pub fn migrate(&self) -> Effect<(), String> {
    let journal = self.journal();
    let mut schema: Effect<u64, SqlError> = id_effect::succeed(0);
    for statement in SCHEMA {
      let db = Arc::clone(&self.db);
      schema = schema.flat_map(move |_| db.execute(statement, &[]));
    }
    schema
      .map_err(|e: SqlError| e.to_string())
      .flat_map(move |_| {
        journal
          .create_table()
          .map_err(|e: EventStoreError| e.to_string())
      })
  }

  /// Creates `room` with `owner` as its first member: `false` when the room exists.
  pub fn create(&self, room: &str, owner: &str) -> Effect<bool, SqlError> {
    self
      .db
      .execute(CREATE_ROOM, &params![room, owner])
      .map(|created| created == 1)
  }

  /// Adds `user` to `room` (nothing when already a member).
  pub fn add_member(&self, room: &str, user: &str) -> Effect<(), SqlError> {
    self
      .db
      .execute(ADD_MEMBER, &params![room, user])
      .map(|_| ())
  }

  /// Whether `user` is a member of `room`.
  pub fn is_member(&self, room: &str, user: &str) -> Effect<bool, SqlError> {
    self
      .db
      .query(IS_MEMBER, &params![room, user])
      .flat_map(|rows| match rows.first().map(|row| row.get::<i64>(0)) {
        Some(Ok(n)) => succeed(n > 0),
        Some(Err(e)) => fail(e),
        None => succeed(false),
      })
  }
}

/// The join policy of the rooms' channels: a signed-in member of the room, nobody else.
#[derive(Clone, Debug)]
pub struct MembersOnly {
  rooms: Rooms,
}

impl MembersOnly {
  /// Members of `rooms` only.
  pub fn new(rooms: Rooms) -> Self {
    Self { rooms }
  }
}

impl JoinPolicy for MembersOnly {
  fn decide(&self, join: &JoinRequest) -> Effect<Decision, AuthzError, DynEnv> {
    if !join.principal.is_authenticated() {
      return succeed(Decision::deny("sign in to join a room"));
    }
    let Some(room) = room_of(&join.topic) else {
      return succeed(Decision::deny("not a room topic"));
    };
    let lookup = self
      .rooms
      .is_member(room, join.principal.id())
      .map_err(|e| AuthzError::Evaluation(e.to_string()));
    on_dyn(lookup).map(|member| Decision::allow_if(member, "not a member of the room"))
  }
}

/// Runs an environment-free effect (a database call) inside a handler's `DynEnv` effect, on the
/// calling fiber.
pub fn on_dyn<A: Send + 'static, E: Send + 'static>(effect: Effect<A, E>) -> Effect<A, E, DynEnv> {
  Effect::from_ctx(move |ctx: Ctx<DynEnv>| async move { ctx.bind(effect).await })
}

#[cfg(test)]
mod tests {
  use id_effect::Exit;
  use id_effect_kit::authz::{BasicPrincipal, CurrentPrincipal};
  use id_effect_kit::sql::TestSqlClient;

  use super::*;

  fn decide(topic: &str, principal: CurrentPrincipal) -> Decision {
    let policy = MembersOnly::new(Rooms::new(Arc::new(TestSqlClient::new())));
    let join = JoinRequest {
      topic: topic.to_owned(),
      principal,
      meta: serde_json::Value::Null,
    };
    match policy.decide(&join).provide_all(DynEnv::new()).block_on() {
      Exit::Success(decision) => decision,
      Exit::Failure(cause) => panic!("{cause:?}"),
    }
  }

  #[test]
  fn anonymous_callers_and_other_topics_are_denied_before_any_lookup() {
    assert!(matches!(
      decide("room:general", CurrentPrincipal::anonymous()),
      Decision::Deny(_)
    ));
    let ada = CurrentPrincipal::new(BasicPrincipal::new("ada"));
    assert!(matches!(decide("lobby", ada), Decision::Deny(_)));
  }
}
