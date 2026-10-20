//! BehaviorRegistry for remote spawn (FLAME model).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use id_effect::{Cause, Exit};
use id_effect::{Deferred, Effect, from_async};
use id_effect_process::{
  CallError, ExitReason, Info, Mailbox, OfferError, Pid, ProcMsg, Process, ProcessNode,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::envelope::{ExitReasonWire, RemoteFault};

/// Spawn fn for a registered behavior.
pub type SpawnFn =
  Arc<dyn Fn(&ProcessNode, &[u8], Option<&Pid>) -> Result<Pid, RemoteFault> + Send + Sync>;

/// Wire bytes into a mailbox.
pub type WireFn = Arc<dyn Fn(&[u8]) -> Result<(), RemoteFault> + Send + Sync>;

/// The encoded reply of a delivered call: resolves once the process answers (or exits).
///
/// Dropping it abandons the wait (the process may still handle the call: at-most-once).
pub type ReplyFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, RemoteFault>> + Send>>;

/// Decode call bytes and enqueue the call; the returned future awaits the encoded reply.
pub type CallWireFn = Arc<dyn Fn(&[u8]) -> Result<ReplyFuture, RemoteFault> + Send + Sync>;

#[derive(Clone)]
struct Route {
  cast: WireFn,
  info: WireFn,
  call: CallWireFn,
}

/// Named spawn factory.
#[derive(Clone)]
pub struct BehaviorEntry {
  /// Stable cluster-wide behavior name.
  pub name: String,
  /// Decode args, spawn, install route.
  pub spawn: SpawnFn,
}

/// Behavior name map + live pid routes.
///
/// Remote delivery respects the target's mailbox bound (M10): a call to a full mailbox faults
/// with [`RemoteFault::MailboxFull`]; a cast or info to a full mailbox is dropped and counted in
/// [`BehaviorRegistry::dropped_full`].
#[derive(Clone, Default)]
pub struct BehaviorRegistry {
  entries: HashMap<String, BehaviorEntry>,
  routes: Arc<Mutex<HashMap<Pid, Route>>>,
  dropped: Arc<AtomicU64>,
}

impl BehaviorRegistry {
  /// Empty registry.
  pub fn new() -> Self {
    Self::default()
  }

  /// Look up spawn entry.
  pub fn get(&self, name: &str) -> Option<&BehaviorEntry> {
    self.entries.get(name)
  }

  /// Remote casts and infos dropped because their target's mailbox was full.
  pub fn dropped_full(&self) -> u64 {
    self.dropped.load(Ordering::Relaxed)
  }

  /// True if pid has a wire route.
  pub fn has_route(&self, pid: &Pid) -> bool {
    self.routes.lock().expect("routes").contains_key(pid)
  }

  /// Spawn named behavior.
  pub fn spawn(
    &self,
    node: &ProcessNode,
    behavior: &str,
    args: &[u8],
    link_to: Option<&Pid>,
  ) -> Result<Pid, RemoteFault> {
    let entry = self
      .get(behavior)
      .ok_or_else(|| RemoteFault::UnknownBehavior(behavior.to_string()))?;
    (entry.spawn)(node, args, link_to)
  }

  /// Deliver cast bytes. A cast to a full mailbox is dropped and counted (still `Ok`).
  pub fn deliver_cast(&self, pid: &Pid, bytes: &[u8]) -> Result<(), RemoteFault> {
    let route = self
      .routes
      .lock()
      .expect("routes")
      .get(pid)
      .cloned()
      .ok_or(RemoteFault::NoProc)?;
    (route.cast)(bytes)
  }

  /// Deliver info bytes. An info to a full mailbox is dropped and counted (still `Ok`).
  pub fn deliver_info(&self, pid: &Pid, bytes: &[u8]) -> Result<(), RemoteFault> {
    let route = self
      .routes
      .lock()
      .expect("routes")
      .get(pid)
      .cloned()
      .ok_or(RemoteFault::NoProc)?;
    (route.info)(bytes)
  }

  /// Deliver call bytes.
  ///
  /// The call is decoded and enqueued now (so it is ordered after earlier casts), or refused
  /// with [`RemoteFault::MailboxFull`] when the target's mailbox is full; the returned
  /// effect awaits the encoded reply. It is `Send` and interruptible: interrupting it abandons
  /// the wait only (at-most-once). Bound it with a timeout — a process that exits without
  /// replying never answers.
  pub fn deliver_call<R: Send + Sync + 'static>(
    &self,
    pid: &Pid,
    bytes: &[u8],
  ) -> Result<Effect<Vec<u8>, RemoteFault, R>, RemoteFault> {
    let route = self
      .routes
      .lock()
      .expect("routes")
      .get(pid)
      .cloned()
      .ok_or(RemoteFault::NoProc)?;
    let reply = (route.call)(bytes)?;
    Ok(from_async(move || reply))
  }

  /// Forget dead pid.
  pub fn forget(&self, pid: &Pid) {
    self.routes.lock().expect("routes").remove(pid);
  }

  /// Register behavior P under name.
  pub fn register_counter_like<P>(&mut self, name: impl Into<String>)
  where
    P: Process,
    P::Args: Serialize + DeserializeOwned + Send + 'static,
    P::Env: Default + Send + 'static,
    P::Call: Serialize + DeserializeOwned + Send + 'static,
    P::Reply: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    P::Cast: Serialize + DeserializeOwned + Send + 'static,
    P::Info: Serialize + DeserializeOwned + Send + 'static,
  {
    self.register::<P>(name);
  }

  /// Register behavior P under name.
  pub fn register<P>(&mut self, name: impl Into<String>)
  where
    P: Process,
    P::Args: Serialize + DeserializeOwned + Send + 'static,
    P::Env: Default + Send + 'static,
    P::Call: Serialize + DeserializeOwned + Send + 'static,
    P::Reply: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    P::Cast: Serialize + DeserializeOwned + Send + 'static,
    P::Info: Serialize + DeserializeOwned + Send + 'static,
  {
    let name = name.into();
    let routes = Arc::clone(&self.routes);
    let dropped = Arc::clone(&self.dropped);
    let spawn: SpawnFn = Arc::new(move |node, args_bytes, link_to| {
      let args: P::Args =
        postcard::from_bytes(args_bytes).map_err(|e| RemoteFault::Decode(e.to_string()))?;
      let addr = match link_to {
        Some(parent) => node.spawn_linked_to::<P>(parent, args, P::Env::default()),
        None => node.spawn::<P>(args, P::Env::default()),
      };
      install_route::<P>(&routes, &dropped, addr.pid().clone(), addr.clone_mailbox());
      Ok(addr.pid().clone())
    });
    self
      .entries
      .insert(name.clone(), BehaviorEntry { name, spawn });
  }

  /// Adopt local process for remote delivery.
  pub fn adopt_local<P>(&self, name: &str, pid: &Pid, mailbox: Mailbox<ProcMsg<P>>)
  where
    P: Process,
    P::Call: Serialize + DeserializeOwned + Send + 'static,
    P::Reply: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
    P::Cast: Serialize + DeserializeOwned + Send + 'static,
    P::Info: Serialize + DeserializeOwned + Send + 'static,
  {
    if self.entries.contains_key(name) {
      install_route::<P>(&self.routes, &self.dropped, pid.clone(), mailbox);
    }
  }
}

/// A cast or info offer's outcome: dropped and counted when the mailbox is full.
fn offered(result: Result<(), OfferError>, dropped: &AtomicU64) -> Result<(), RemoteFault> {
  match result {
    Ok(()) => Ok(()),
    Err(OfferError::Full) => {
      dropped.fetch_add(1, Ordering::Relaxed);
      Ok(())
    }
    Err(_) => Err(RemoteFault::NoProc),
  }
}

fn install_route<P>(
  routes: &Arc<Mutex<HashMap<Pid, Route>>>,
  dropped: &Arc<AtomicU64>,
  pid: Pid,
  mailbox: Mailbox<ProcMsg<P>>,
) where
  P: Process,
  P::Call: Serialize + DeserializeOwned + Send + 'static,
  P::Reply: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
  P::Cast: Serialize + DeserializeOwned + Send + 'static,
  P::Info: Serialize + DeserializeOwned + Send + 'static,
{
  let cast: WireFn = {
    let mailbox = mailbox.clone();
    let dropped = Arc::clone(dropped);
    Arc::new(move |bytes| {
      let msg: P::Cast =
        postcard::from_bytes(bytes).map_err(|e| RemoteFault::Decode(e.to_string()))?;
      offered(mailbox.try_offer_now(ProcMsg::Cast(msg)), &dropped)
    })
  };
  let info: WireFn = {
    let mailbox = mailbox.clone();
    let dropped = Arc::clone(dropped);
    Arc::new(move |bytes| {
      let msg: P::Info =
        postcard::from_bytes(bytes).map_err(|e| RemoteFault::Decode(e.to_string()))?;
      offered(
        mailbox.try_offer_now(ProcMsg::Info(Info::User(msg))),
        &dropped,
      )
    })
  };
  let call: CallWireFn = {
    let mailbox = mailbox.clone();
    Arc::new(move |bytes| {
      let msg: P::Call =
        postcard::from_bytes(bytes).map_err(|e| RemoteFault::Decode(e.to_string()))?;
      let typed: Deferred<P::Reply, CallError> = Deferred::new();
      let wait = typed.wait_future();
      match mailbox.try_offer_now(ProcMsg::Call(msg, typed)) {
        Ok(()) => {}
        Err(OfferError::Full) => return Err(RemoteFault::MailboxFull),
        Err(_) => return Err(RemoteFault::NoProc),
      }
      let reply: ReplyFuture = Box::pin(async move { encode_reply(wait.await) });
      Ok(reply)
    })
  };
  routes
    .lock()
    .expect("routes")
    .insert(pid, Route { cast, info, call });
}

/// Encode a process's typed call outcome as the wire reply.
fn encode_reply<A: Serialize>(exit: Exit<A, CallError>) -> Result<Vec<u8>, RemoteFault> {
  match exit {
    Exit::Success(rep) => {
      postcard::to_allocvec(&rep).map_err(|e| RemoteFault::Decode(e.to_string()))
    }
    Exit::Failure(Cause::Fail(CallError::Down(reason))) => Err(RemoteFault::Down((&reason).into())),
    Exit::Failure(Cause::Fail(CallError::Timeout)) => Err(RemoteFault::Timeout),
    Exit::Failure(Cause::Fail(CallError::MailboxClosed)) => Err(RemoteFault::NoProc),
    Exit::Failure(Cause::Fail(CallError::MailboxFull)) => Err(RemoteFault::MailboxFull),
    Exit::Failure(_) => {
      let wire: ExitReasonWire = (&ExitReason::Interrupted).into();
      Err(RemoteFault::Down(wire))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect::Never;
  use id_effect::{HNil, succeed, sync};
  use id_effect_process::{Next, NodeName, ProcessCtx};

  struct Echo {
    n: u32,
  }

  impl Process for Echo {
    type Args = u32;
    type Env = ();
    type Call = u32;
    type Reply = u32;
    type Cast = u32;
    type Info = u32;
    type Error = Never;

    fn init(args: u32, _ctx: ProcessCtx) -> Effect<Self, Never, ()> {
      succeed(Echo { n: args })
    }

    fn handle_call(mut self, msg: u32, _ctx: ProcessCtx) -> Effect<(u32, Next<Self>), Never, ()> {
      sync(move || {
        self.n += msg;
        (self.n, Next::Continue(self))
      })
    }
  }

  fn pid(n: u64) -> Pid {
    Pid::from_parts(NodeName::new("t@test"), n, 1)
  }

  fn registry() -> BehaviorRegistry {
    let mut reg = BehaviorRegistry::new();
    reg.register::<Echo>("echo");
    reg
  }

  fn mailbox() -> Mailbox<ProcMsg<Echo>> {
    Mailbox::new()
  }

  /// The next mailbox message (it must already be there).
  fn take(q: &Mailbox<ProcMsg<Echo>>) -> ProcMsg<Echo> {
    q.take::<HNil>().block_on().into_result().expect("take")
  }

  /// Deliver a call; the reply effect is checked by the caller.
  fn call(
    reg: &BehaviorRegistry,
    pid: &Pid,
    bytes: &[u8],
  ) -> Result<Effect<Vec<u8>, RemoteFault, HNil>, RemoteFault> {
    reg.deliver_call(pid, bytes)
  }

  fn enc(n: u32) -> Vec<u8> {
    postcard::to_allocvec(&n).expect("encode")
  }

  /// Adopt a bare mailbox, deliver a call, then complete the typed reply with `typed_outcome`.
  fn call_outcome(
    typed_outcome: impl FnOnce(&Deferred<u32, CallError>),
  ) -> Result<Vec<u8>, Cause<RemoteFault>> {
    let reg = registry();
    let q = mailbox();
    reg.adopt_local::<Echo>("echo", &pid(1), q.clone());
    let reply = call(&reg, &pid(1), &enc(3)).unwrap_or_else(|f| panic!("deliver: {f:?}"));
    match take(&q) {
      ProcMsg::Call(msg, typed) => {
        assert_eq!(msg, 3);
        typed_outcome(&typed);
      }
      _ => panic!("expected a call"),
    }
    reply.block_on().into_result()
  }

  #[test]
  fn unknown_behavior_and_unrouted_pids_are_refused() {
    let reg = registry();
    let node = ProcessNode::new(NodeName::new("t@test"));
    assert!(matches!(
      reg.spawn(&node, "nope", &enc(0), None),
      Err(RemoteFault::UnknownBehavior(n)) if n == "nope"
    ));
    assert!(reg.get("echo").is_some_and(|e| e.name == "echo"));
    assert_eq!(reg.deliver_cast(&pid(9), &enc(1)), Err(RemoteFault::NoProc));
    assert_eq!(reg.deliver_info(&pid(9), &enc(1)), Err(RemoteFault::NoProc));
    assert_eq!(
      call(&reg, &pid(9), &enc(1)).err(),
      Some(RemoteFault::NoProc)
    );
  }

  #[test]
  fn register_counter_like_is_register() {
    let mut reg = BehaviorRegistry::new();
    reg.register_counter_like::<Echo>("counter");
    assert!(reg.get("counter").is_some());
    assert!(reg.get("echo").is_none());
  }

  #[test]
  fn spawn_rejects_undecodable_args() {
    let reg = registry();
    let node = ProcessNode::new(NodeName::new("t@test"));
    assert!(matches!(
      reg.spawn(&node, "echo", &[], None),
      Err(RemoteFault::Decode(_))
    ));
  }

  #[test]
  fn spawned_behavior_answers_wire_calls() {
    let reg = registry();
    let node = ProcessNode::new(NodeName::new("t@test"));
    let pid = reg.spawn(&node, "echo", &enc(10), None).expect("spawn");
    assert!(reg.has_route(&pid));
    reg.deliver_cast(&pid, &enc(1)).expect("cast");
    reg.deliver_info(&pid, &enc(1)).expect("info");
    let reply = call(&reg, &pid, &enc(5)).unwrap_or_else(|f| panic!("call: {f:?}"));
    let bytes = reply.block_on().into_result().expect("reply");
    assert_eq!(postcard::from_bytes::<u32>(&bytes).unwrap(), 15);
    reg.forget(&pid);
    assert!(!reg.has_route(&pid));
    assert_eq!(reg.deliver_cast(&pid, &enc(1)), Err(RemoteFault::NoProc));
  }

  #[test]
  fn spawn_linked_to_a_parent_installs_a_route() {
    let reg = registry();
    let node = ProcessNode::new(NodeName::new("t@test"));
    let parent = reg.spawn(&node, "echo", &enc(0), None).expect("parent");
    let child = reg
      .spawn(&node, "echo", &enc(0), Some(&parent))
      .expect("child");
    assert_ne!(parent, child);
    assert!(reg.has_route(&child));
  }

  #[test]
  fn adopt_local_requires_a_registered_behavior() {
    let reg = registry();
    reg.adopt_local::<Echo>("other", &pid(1), mailbox());
    assert!(!reg.has_route(&pid(1)));
    reg.adopt_local::<Echo>("echo", &pid(1), mailbox());
    assert!(reg.has_route(&pid(1)));
  }

  #[test]
  fn adopted_mailbox_receives_typed_cast_and_info() {
    let reg = registry();
    let q = mailbox();
    reg.adopt_local::<Echo>("echo", &pid(1), q.clone());
    reg.deliver_cast(&pid(1), &enc(4)).expect("cast");
    reg.deliver_info(&pid(1), &enc(6)).expect("info");
    assert!(matches!(take(&q), ProcMsg::Cast(4)));
    assert!(matches!(take(&q), ProcMsg::Info(Info::User(6))));
  }

  #[test]
  fn undecodable_wire_bytes_fail_at_the_boundary() {
    let reg = registry();
    reg.adopt_local::<Echo>("echo", &pid(1), mailbox());
    assert!(matches!(
      reg.deliver_cast(&pid(1), &[]),
      Err(RemoteFault::Decode(_))
    ));
    assert!(matches!(
      reg.deliver_info(&pid(1), &[]),
      Err(RemoteFault::Decode(_))
    ));
    assert!(matches!(
      call(&reg, &pid(1), &[]).err(),
      Some(RemoteFault::Decode(_))
    ));
  }

  #[test]
  fn closed_mailbox_is_noproc() {
    let reg = registry();
    let q = mailbox();
    reg.adopt_local::<Echo>("echo", &pid(1), q.clone());
    q.close();
    assert_eq!(reg.deliver_cast(&pid(1), &enc(1)), Err(RemoteFault::NoProc));
    assert_eq!(reg.deliver_info(&pid(1), &enc(1)), Err(RemoteFault::NoProc));
    assert_eq!(
      call(&reg, &pid(1), &enc(1)).err(),
      Some(RemoteFault::NoProc)
    );
  }

  /// M10: remote delivery to a full mailbox faults a call and drops + counts a cast or info.
  #[test]
  fn m10_remote_send_to_full_mailbox_faults() {
    let reg = registry();
    let q: Mailbox<ProcMsg<Echo>> = Mailbox::with_capacity(1);
    reg.adopt_local::<Echo>("echo", &pid(1), q.clone());
    reg.deliver_cast(&pid(1), &enc(1)).expect("first cast fits");
    assert_eq!(reg.dropped_full(), 0);
    assert_eq!(
      call(&reg, &pid(1), &enc(2)).err(),
      Some(RemoteFault::MailboxFull)
    );
    assert_eq!(reg.deliver_cast(&pid(1), &enc(3)), Ok(()));
    assert_eq!(reg.deliver_info(&pid(1), &enc(4)), Ok(()));
    assert_eq!(reg.dropped_full(), 2, "the cast and the info were dropped");
    assert_eq!(q.len(), 1, "only the first cast is buffered");
    assert!(matches!(take(&q), ProcMsg::Cast(1)));
    // Room again: the next call is enqueued.
    assert!(call(&reg, &pid(1), &enc(5)).is_ok());
    assert_eq!(RemoteFault::MailboxFull.to_string(), "mailbox full");
  }

  #[test]
  fn typed_reply_is_encoded_onto_the_wire() {
    let bytes = call_outcome(|typed| {
      assert!(typed.try_succeed(42));
    })
    .expect("reply");
    assert_eq!(postcard::from_bytes::<u32>(&bytes).unwrap(), 42);
  }

  #[test]
  fn call_errors_map_to_remote_faults() {
    let cases = [
      (CallError::Timeout, RemoteFault::Timeout),
      (CallError::MailboxClosed, RemoteFault::NoProc),
      (CallError::MailboxFull, RemoteFault::MailboxFull),
      (
        CallError::Down(ExitReason::Shutdown),
        RemoteFault::Down((&ExitReason::Shutdown).into()),
      ),
    ];
    for (err, want) in cases {
      let got = call_outcome(move |typed| {
        assert!(typed.try_fail_cause(Cause::fail(err)));
      });
      assert!(matches!(got, Err(Cause::Fail(f)) if f == want));
    }
  }

  #[test]
  fn call_defect_maps_to_interrupted_down() {
    let got = call_outcome(|typed| {
      assert!(typed.try_fail_cause(Cause::die("boom")));
    });
    let want = RemoteFault::Down((&ExitReason::Interrupted).into());
    assert!(matches!(got, Err(Cause::Fail(f)) if f == want));
  }
}
