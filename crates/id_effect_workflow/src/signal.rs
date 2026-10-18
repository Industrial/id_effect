//! Signals (release 0.11.0 decision D6): [`WorkflowClient::signal`] stores a named, encoded
//! payload for an instance in the `workflow_signals` table and wakes it;
//! [`WorkflowContext::wait_signal`] receives the signals of one name in arrival order, including
//! the ones sent before it waited.

use std::fmt;
use std::sync::Arc;

use id_effect::{Effect, WallClock, fail, succeed, suspend};

use crate::WorkflowName;
use crate::error::WorkflowError;
use crate::history::{EventKind, WorkflowStore};
use crate::workflow::{Journaled, Position, WorkflowContext, WorkflowId, decode, encode};

/// Sends signals to workflow instances (the producer side; no workflow needs to be registered).
/// Cloning shares the store. [`WorkflowEngine::client`](crate::WorkflowEngine::client) returns
/// one over the engine's store and clock.
#[derive(Clone)]
pub struct WorkflowClient {
  store: Arc<dyn WorkflowStore>,
  clock: WallClock,
}

impl fmt::Debug for WorkflowClient {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("WorkflowClient")
      .field("clock", &self.clock)
      .finish_non_exhaustive()
  }
}

impl WorkflowClient {
  /// A client over `store`, on the system clock.
  pub fn new(store: impl WorkflowStore) -> Self {
    Self::from_arc(Arc::new(store))
  }

  /// A client over a shared store.
  pub fn from_arc(store: Arc<dyn WorkflowStore>) -> Self {
    Self {
      store,
      clock: WallClock::system(),
    }
  }

  /// Stamps signals with `clock`'s time instead of the system clock's.
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// Sends signal `name` (a valid [`WorkflowName`]) with `payload` to the running instance
  /// `instance`: stored durably, numbered in arrival order, and the instance woken if it
  /// waits. `false` when no running instance has the id (absent or finished); the payload is
  /// then dropped. A signal nobody waits for stays stored with the instance.
  pub fn signal<P: Journaled>(
    &self,
    instance: &WorkflowId,
    name: impl Into<String>,
    payload: P,
  ) -> Effect<bool, WorkflowError> {
    let name = name.into();
    if let Err(e) = WorkflowName::new(name.as_str()) {
      return fail(e.into());
    }
    let (store, instance, payload) = (Arc::clone(&self.store), instance.clone(), encode(payload));
    self.clock.now().flat_map(move |now| {
      store
        .signal(&instance, &name, payload, now)
        .map(|number| number.is_some())
    })
  }
}

impl WorkflowContext {
  /// The next signal `name` sent to this instance, in arrival order, decoded as `P`, journaled.
  ///
  /// Signals are buffered: one sent before the call (even before the instance first ran) is
  /// returned at once. With none pending the run parks ([`WorkflowError::Waiting`]) until
  /// [`WorkflowClient::signal`] sends one. Each call consumes one signal: the n-th call for
  /// `name` receives the n-th signal of that name. On replay the journaled signal is returned.
  /// A payload that does not decode as `P` fails the call with [`WorkflowError::Decode`].
  pub fn wait_signal<P: Journaled>(&self, name: impl Into<String>) -> Effect<P, WorkflowError> {
    let name = name.into();
    if let Err(e) = WorkflowName::new(name.as_str()) {
      return fail(e.into());
    }
    let ctx = self.clone();
    suspend(move || match ctx.position(EventKind::Signal, &name) {
      Err(e) => fail(e),
      Ok(Position::Replayed(payload)) => ctx.received::<P>(&name, &payload),
      Ok(Position::Fresh(seq)) => {
        let after = ctx.last_signal(&name);
        let store = Arc::clone(&ctx.inner.store);
        store
          .next_signal(ctx.instance(), &name, after)
          .flat_map(move |next| match next {
            None => ctx.park(),
            Some(signal) => {
              let payload = serde_json::json!({
                "number": signal.number,
                "payload": signal.payload,
              });
              let journaled = payload.clone();
              let record = ctx.record(seq, EventKind::Signal, name.clone(), payload);
              record.flat_map(move |()| ctx.received::<P>(&name, &journaled))
            }
          })
      }
    })
  }

  /// A journaled signal payload: notes its number and decodes its value.
  fn received<P: Journaled>(
    &self,
    name: &str,
    payload: &serde_json::Value,
  ) -> Effect<P, WorkflowError> {
    let what = format!("signal {name}");
    let Some(number) = payload.get("number").and_then(serde_json::Value::as_i64) else {
      return fail(WorkflowError::Decode {
        what,
        message: format!("malformed journal payload {payload}"),
      });
    };
    self.received_signal(name, number);
    let value = payload
      .get("payload")
      .cloned()
      .unwrap_or(serde_json::Value::Null);
    match decode::<P>(&what, &value) {
      Ok(p) => succeed(p),
      Err(e) => fail(e),
    }
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit, UtcDateTime};

  use super::*;
  use crate::history::NewInstance;
  use crate::lease::{EngineId, Lease};
  use crate::{MemoryWorkflowStore, WorkflowNameError};

  #[derive(Clone, Debug, PartialEq, id_effect::Schema)]
  struct Note {
    n: i64,
  }

  fn run<A: Send + 'static>(eff: Effect<A, WorkflowError>) -> Result<A, WorkflowError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn context(store: &MemoryWorkflowStore) -> WorkflowContext {
    let lease = Lease {
      instance: WorkflowId::new("i").unwrap(),
      owner: EngineId::new("e"),
      token: 1,
      expires_at: UtcDateTime::unsafe_make(0),
    };
    WorkflowContext::new(
      lease,
      Vec::new(),
      Arc::new(store.clone()),
      None,
      WallClock::system(),
      id_effect::Random::seeded(1),
      std::time::Duration::from_secs(1),
    )
  }

  #[test]
  fn the_client_validates_names_and_reports_absent_instances() {
    let store = MemoryWorkflowStore::new();
    let client = WorkflowClient::new(store.clone());
    let i = WorkflowId::new("i").unwrap();
    assert_eq!(run(client.signal(&i, "go", Note { n: 1 })), Ok(false));
    let new = NewInstance {
      id: i.clone(),
      name: "w".into(),
      version: "1".into(),
      input: serde_json::json!({}),
    };
    assert_eq!(
      run(store.create(new, UtcDateTime::unsafe_make(0))),
      Ok(true)
    );
    assert_eq!(run(client.signal(&i, "go", Note { n: 1 })), Ok(true));
    assert_eq!(
      run(client.signal(&i, "", Note { n: 1 })),
      Err(WorkflowError::InvalidName(WorkflowNameError::Empty))
    );
    assert!(format!("{client:?}").contains("WorkflowClient"));
  }

  #[test]
  fn received_signals_decode_or_report() {
    let ctx = context(&MemoryWorkflowStore::new());
    let good = serde_json::json!({ "number": 4, "payload": { "n": 9 } });
    assert_eq!(run(ctx.received::<Note>("go", &good)), Ok(Note { n: 9 }));
    assert_eq!(ctx.last_signal("go"), 4);
    assert_eq!(ctx.last_signal("other"), 0);
    let malformed = serde_json::json!({ "payload": { "n": 9 } });
    assert!(matches!(
      run(ctx.received::<Note>("go", &malformed)),
      Err(WorkflowError::Decode { .. })
    ));
    let wrong = serde_json::json!({ "number": 5, "payload": "x" });
    assert!(matches!(
      run(ctx.received::<Note>("go", &wrong)),
      Err(WorkflowError::Decode { .. })
    ));
    assert_eq!(
      run(ctx.wait_signal::<Note>("not valid")).unwrap_err(),
      WorkflowError::InvalidName(WorkflowNameError::InvalidChar { ch: ' ', index: 3 })
    );
  }

  #[test]
  fn a_parked_context_fails_every_later_call() {
    let ctx = context(&MemoryWorkflowStore::new());
    assert!(!ctx.is_parked());
    assert_eq!(run(ctx.park::<()>()), Err(WorkflowError::Waiting));
    assert!(ctx.is_parked());
    assert_eq!(run(ctx.now()).unwrap_err(), WorkflowError::Waiting);
    assert_eq!(
      run(ctx.wait_signal::<Note>("go")).unwrap_err(),
      WorkflowError::Waiting
    );
    assert_eq!(
      run(ctx.sleep(std::time::Duration::from_secs(1))).unwrap_err(),
      WorkflowError::Waiting
    );
  }

  #[test]
  fn without_a_job_queue_timers_and_activities_cannot_be_scheduled() {
    let ctx = context(&MemoryWorkflowStore::new());
    assert_eq!(ctx.jobs().err(), Some(WorkflowError::NoJobQueue));
  }
}
