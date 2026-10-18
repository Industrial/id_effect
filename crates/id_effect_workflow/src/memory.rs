//! [`MemoryWorkflowStore`]: the [`WorkflowStore`] in process memory, for unit tests and
//! examples. Same lease and fencing rules as the Postgres store, one mutex instead of row locks.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, UtcDateTime, sync};

use crate::error::WorkflowError;
use crate::history::{
  Claimed, Finish, HistoryEvent, InstanceRecord, NewInstance, WorkflowStatus, WorkflowStore,
};
use crate::lease::{EngineId, Fenced, Lease};
use crate::workflow::WorkflowId;

#[derive(Default)]
struct State {
  instances: BTreeMap<WorkflowId, InstanceRecord>,
  history: BTreeMap<WorkflowId, Vec<HistoryEvent>>,
}

/// [`WorkflowStore`] in memory. Clones share the state, so two engines over clones of one store
/// behave like two engines over one database (but nothing survives the process).
#[derive(Clone, Default)]
pub struct MemoryWorkflowStore {
  state: Arc<Mutex<State>>,
}

impl fmt::Debug for MemoryWorkflowStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryWorkflowStore")
      .field("instances", &self.lock().instances.len())
      .finish()
  }
}

impl MemoryWorkflowStore {
  /// An empty store.
  pub fn new() -> Self {
    Self::default()
  }

  fn lock(&self) -> MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// `f` on the locked state, as an effect run when it is executed.
  fn with<A: Send + 'static>(
    &self,
    f: impl FnOnce(&mut State) -> A + Send + 'static,
  ) -> Effect<A, WorkflowError> {
    let state = Arc::clone(&self.state);
    sync(move || f(&mut state.lock().unwrap_or_else(PoisonError::into_inner)))
  }
}

/// Whether `lease` is the instance's current lease (owner, token, running).
fn holds(record: &InstanceRecord, lease: &Lease) -> bool {
  record.status == WorkflowStatus::Running
    && record.fencing_token == lease.token
    && record.lease_owner.as_ref() == Some(&lease.owner)
}

/// Whether `record` is running with a free or expired lease at `now`.
fn claimable(record: &InstanceRecord, now: UtcDateTime) -> bool {
  record.status == WorkflowStatus::Running && record.lease_expires_at.is_none_or(|at| at < now)
}

/// Takes the lease of `record` (checked claimable).
fn take(
  record: &mut InstanceRecord,
  owner: &EngineId,
  now: UtcDateTime,
  expires_at: UtcDateTime,
) -> Claimed {
  record.fencing_token += 1;
  record.lease_owner = Some(owner.clone());
  record.lease_expires_at = Some(expires_at);
  record.updated_at = now;
  Claimed {
    lease: Lease {
      instance: record.id.clone(),
      owner: owner.clone(),
      token: record.fencing_token,
      expires_at,
    },
    name: record.name.clone(),
    version: record.version.clone(),
    input: record.input.clone(),
  }
}

impl WorkflowStore for MemoryWorkflowStore {
  fn create(&self, instance: NewInstance, now: UtcDateTime) -> Effect<bool, WorkflowError> {
    self.with(move |state| {
      if state.instances.contains_key(&instance.id) {
        return false;
      }
      let record = InstanceRecord {
        id: instance.id.clone(),
        name: instance.name,
        version: instance.version,
        status: WorkflowStatus::Running,
        input: instance.input,
        output: None,
        error: None,
        lease_owner: None,
        lease_expires_at: None,
        fencing_token: 0,
        created_at: now,
        updated_at: now,
      };
      state.instances.insert(instance.id, record);
      true
    })
  }

  fn get(&self, id: &WorkflowId) -> Effect<Option<InstanceRecord>, WorkflowError> {
    let id = id.clone();
    self.with(move |state| state.instances.get(&id).cloned())
  }

  fn acquire(
    &self,
    id: &WorkflowId,
    owner: &EngineId,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Option<Claimed>, WorkflowError> {
    let (id, owner) = (id.clone(), owner.clone());
    self.with(move |state| {
      let record = state.instances.get_mut(&id)?;
      claimable(record, now).then(|| take(record, &owner, now, expires_at))
    })
  }

  fn claim(
    &self,
    name: &str,
    owner: &EngineId,
    limit: u16,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Vec<Claimed>, WorkflowError> {
    let (name, owner) = (name.to_owned(), owner.clone());
    self.with(move |state| {
      let mut free: Vec<&mut InstanceRecord> = state
        .instances
        .values_mut()
        .filter(|r| r.name == name && claimable(r, now))
        .collect();
      free.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
      free
        .into_iter()
        .take(usize::from(limit))
        .map(|record| take(record, &owner, now, expires_at))
        .collect()
    })
  }

  fn history(&self, id: &WorkflowId) -> Effect<Vec<HistoryEvent>, WorkflowError> {
    let id = id.clone();
    self.with(move |state| state.history.get(&id).cloned().unwrap_or_default())
  }

  fn append(
    &self,
    lease: &Lease,
    event: HistoryEvent,
    extend_to: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    let lease = lease.clone();
    self.with(move |state| {
      let now = event.recorded_at;
      let Some(record) = state.instances.get_mut(&lease.instance) else {
        return Fenced::Lost;
      };
      if !holds(record, &lease) || record.lease_expires_at.is_none_or(|at| at <= now) {
        return Fenced::Lost;
      }
      record.lease_expires_at = Some(extend_to);
      record.updated_at = now;
      let events = state.history.entry(lease.instance).or_default();
      if events.iter().any(|e| e.seq == event.seq) {
        return Fenced::Lost;
      }
      events.push(event);
      events.sort_by_key(|e| e.seq);
      Fenced::Applied
    })
  }

  fn renew(
    &self,
    lease: &Lease,
    now: UtcDateTime,
    expires_at: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    let lease = lease.clone();
    self.with(
      move |state| match state.instances.get_mut(&lease.instance) {
        Some(record) if holds(record, &lease) => {
          record.lease_expires_at = Some(expires_at);
          record.updated_at = now;
          Fenced::Applied
        }
        _ => Fenced::Lost,
      },
    )
  }

  fn finish(
    &self,
    lease: &Lease,
    outcome: Finish,
    now: UtcDateTime,
  ) -> Effect<Fenced, WorkflowError> {
    let lease = lease.clone();
    self.with(
      move |state| match state.instances.get_mut(&lease.instance) {
        Some(record)
          if holds(record, &lease) && record.lease_expires_at.is_some_and(|at| at > now) =>
        {
          match outcome {
            Finish::Completed(output) => {
              record.status = WorkflowStatus::Completed;
              record.output = Some(output);
            }
            Finish::Failed(error) => {
              record.status = WorkflowStatus::Failed;
              record.error = Some(error);
            }
          }
          record.lease_owner = None;
          record.lease_expires_at = None;
          record.updated_at = now;
          Fenced::Applied
        }
        _ => Fenced::Lost,
      },
    )
  }

  fn release(&self, lease: &Lease, now: UtcDateTime) -> Effect<Fenced, WorkflowError> {
    let lease = lease.clone();
    self.with(
      move |state| match state.instances.get_mut(&lease.instance) {
        Some(record) if holds(record, &lease) => {
          record.lease_owner = None;
          record.lease_expires_at = None;
          record.updated_at = now;
          Fenced::Applied
        }
        _ => Fenced::Lost,
      },
    )
  }
}

#[cfg(test)]
mod tests {
  use id_effect::{Cause, Exit};

  use super::*;
  use crate::history::EventKind;

  fn run<A: Send + 'static>(eff: Effect<A, WorkflowError>) -> A {
    match eff.block_on() {
      Exit::Success(a) => a,
      Exit::Failure(Cause::Fail(e)) => panic!("failed: {e}"),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn t(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn event(seq: i64, at: UtcDateTime) -> HistoryEvent {
    HistoryEvent {
      seq,
      kind: EventKind::Step,
      name: format!("s{seq}"),
      payload: serde_json::json!({ "ok": { "n": seq } }),
      recorded_at: at,
    }
  }

  fn started(store: &MemoryWorkflowStore, id: &WorkflowId) {
    let new = NewInstance {
      id: id.clone(),
      name: "w".into(),
      version: "1".into(),
      input: serde_json::json!({}),
    };
    assert!(run(store.create(new.clone(), t(0))));
    assert!(!run(store.create(new, t(0))), "an existing id is kept");
  }

  #[test]
  fn memory_store_fences_stale_writers() {
    let store = MemoryWorkflowStore::new();
    let id = WorkflowId::new("i").unwrap();
    started(&store, &id);
    let (a, b) = (EngineId::new("a"), EngineId::new("b"));

    let la = run(store.acquire(&id, &a, t(0), t(10))).unwrap().lease;
    assert_eq!(la.token, 1);
    assert!(run(store.acquire(&id, &b, t(5), t(15))).is_none(), "held");
    assert_eq!(
      run(store.append(&la, event(1, t(1)), t(11))),
      Fenced::Applied
    );
    assert_eq!(
      run(store.append(&la, event(1, t(2)), t(12))),
      Fenced::Lost,
      "seq taken"
    );
    // A's lease (extended to 12 s) has run out at 13 s: its own append is refused.
    assert_eq!(run(store.append(&la, event(2, t(13)), t(23))), Fenced::Lost);
    let lb = run(store.claim("w", &b, 10, t(13), t(23))).remove(0).lease;
    assert_eq!(lb.token, 2);
    // Even with a clock that says its lease still runs, A's token is stale.
    assert_eq!(run(store.append(&la, event(2, t(5)), t(15))), Fenced::Lost);
    assert_eq!(run(store.renew(&la, t(5), t(15))), Fenced::Lost);
    assert_eq!(
      run(store.finish(&la, Finish::Failed("x".into()), t(5))),
      Fenced::Lost
    );
    assert_eq!(run(store.release(&la, t(5))), Fenced::Lost);
    assert_eq!(
      run(store.append(&lb, event(2, t(14)), t(24))),
      Fenced::Applied
    );
    let seqs: Vec<i64> = run(store.history(&id)).iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [1, 2]);

    assert_eq!(run(store.renew(&lb, t(20), t(30))), Fenced::Applied);
    assert_eq!(
      run(store.finish(&lb, Finish::Completed(serde_json::json!({ "n": 1 })), t(21))),
      Fenced::Applied
    );
    let done = run(store.get(&id)).unwrap();
    assert_eq!(done.status, WorkflowStatus::Completed);
    assert_eq!((done.lease_owner, done.lease_expires_at), (None, None));
    assert!(
      run(store.acquire(&id, &a, t(100), t(110))).is_none(),
      "finished"
    );
  }

  #[test]
  fn claim_takes_the_oldest_free_instances_of_one_workflow() {
    let store = MemoryWorkflowStore::new();
    for (n, name) in [(1, "w"), (2, "other"), (3, "w"), (4, "w")] {
      let new = NewInstance {
        id: WorkflowId::new(format!("i{n}")).unwrap(),
        name: name.into(),
        version: "1".into(),
        input: serde_json::json!({}),
      };
      assert!(run(store.create(new, t(n))));
    }
    let owner = EngineId::new("e");
    let got: Vec<String> = run(store.claim("w", &owner, 2, t(10), t(20)))
      .into_iter()
      .map(|c| c.lease.instance.to_string())
      .collect();
    assert_eq!(got, ["i1", "i3"]);
    assert_eq!(run(store.claim("w", &owner, 5, t(11), t(21))).len(), 1);
    assert!(run(store.claim("w", &owner, 5, t(12), t(22))).is_empty());
    let released = run(store.acquire(&WorkflowId::new("i2").unwrap(), &owner, t(1), t(2))).unwrap();
    assert_eq!(run(store.release(&released.lease, t(1))), Fenced::Applied);
    assert!(
      run(store.get(&released.lease.instance))
        .unwrap()
        .lease_owner
        .is_none()
    );
  }
}
