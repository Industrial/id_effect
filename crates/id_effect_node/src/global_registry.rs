//! [`GlobalRegistry`] — cluster-wide process names (BEAM's `global`).
//!
//! Every node keeps a full replica of the name table. A name is claimed by the node that hosts
//! the process ([`GlobalRegistry::register`] takes local pids only); the claim is broadcast to
//! every peer, so [`GlobalRegistry::whereis`] answers from the local replica on any node. The
//! owner monitors the process and broadcasts the release when it exits, and
//! [`Mesh::peer_down`] drops every claim the lost node held.
//!
//! **Availability over consistency.** Registration checks the local replica only, so the two
//! sides of a partition can each claim a name. When they meet again ([`Mesh::add_peer`] or
//! [`GlobalRegistry::sync_with`]) each node sends the claims it owns, and every node keeps the
//! same winner (the conflict rule, a total order every node evaluates alike):
//!
//! 1. a claim from the node that already owns the name replaces it (the owner's latest word);
//! 2. otherwise the claim whose owner has the lowest `(incarnation, node name)` wins — node
//!    incarnations are boot-time ordered, so the longest-running node keeps the name;
//! 3. ties (impossible across distinct nodes) fall to the lowest `(pid id, pid incarnation)`.
//!
//! The losing claim's owner forgets it and sends the losing process an exit signal whose reason
//! is [`name_conflict`] (BEAM's `{global_name_conflict, Name}`). The signal is an ordinary
//! trappable exit: a process that traps exits keeps running, without the name.
//!
//! Messages are at-most-once like the rest of the mesh; a lost register or release is repaired
//! by the next sync with that peer, which replaces everything the receiver held for the sender.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use id_effect::{Effect, fail, suspend};
use id_effect_process::{Addr, ExitReason, MonitorRef, MonitorSink, Pid, Process};
use serde::{Deserialize, Serialize};

use crate::envelope::{PidWire, ProcessEnvelope, RemoteFault};
use crate::id::NodeId;
use crate::mesh::Mesh;

/// Longest accepted global name, in bytes.
pub const MAX_NAME_LEN: usize = 255;

const NAME_CONFLICT: &str = "NameConflict: ";

/// The exit reason a process receives when it loses `name` to a conflicting registration.
pub fn name_conflict(name: &str) -> ExitReason {
  ExitReason::Error(format!("{NAME_CONFLICT}{name}"))
}

/// The name a [`name_conflict`] exit reason is about, or `None` for any other reason.
pub fn conflicting_name(reason: &ExitReason) -> Option<&str> {
  match reason {
    ExitReason::Error(text) => text.strip_prefix(NAME_CONFLICT),
    _ => None,
  }
}

fn valid_name(name: &str) -> bool {
  !name.is_empty() && name.len() <= MAX_NAME_LEN
}

/// One replicated registration: `name` → `pid`, owned by the node hosting `pid`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameClaim {
  /// The global name.
  pub name: String,
  /// The registered process.
  pub pid: PidWire,
  /// The node that made the claim (the node hosting `pid`).
  pub owner: NodeId,
}

impl NameClaim {
  fn rank(&self) -> (u128, &str, u64, u32) {
    (
      self.owner.incarnation(),
      self.owner.name(),
      self.pid.id,
      self.pid.incarnation,
    )
  }

  /// Whether this claim displaces `held` for the same name (the module's conflict rule).
  pub fn wins_over(&self, held: &NameClaim) -> bool {
    if self.owner.same_node(&held.owner) {
      return true;
    }
    self.rank() < held.rank()
  }

  fn well_formed(&self) -> bool {
    valid_name(&self.name) && self.pid.node == self.owner.name()
  }
}

/// Why a global name operation was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GlobalNameError {
  /// The name is already held (by `owner`, as this node's replica knows it).
  #[error("global name {name:?} is already registered to {owner}")]
  AlreadyRegistered {
    /// The contested name.
    name: String,
    /// The process currently holding it.
    owner: Pid,
  },
  /// The process lives on another node; names are claimed by the hosting node.
  #[error("{0} is not hosted on this node")]
  NotLocal(Pid),
  /// The process is not alive.
  #[error("{0} is not alive")]
  NoProc(Pid),
  /// The name is empty or longer than [`MAX_NAME_LEN`] bytes.
  #[error("global names must be 1..={MAX_NAME_LEN} bytes")]
  InvalidName,
}

/// A claim this node owns: the monitor that releases it and the typed address, if given.
struct LocalClaim {
  pid: Pid,
  monitor: Option<MonitorRef>,
  addr: Option<Box<dyn Any + Send + Sync>>,
}

/// One node's replica of the name table (lives in the [`Mesh`]).
#[derive(Default)]
pub(crate) struct GlobalNames {
  claims: BTreeMap<String, NameClaim>,
  local: HashMap<String, LocalClaim>,
}

impl GlobalNames {
  fn owned_claims(&self) -> Vec<NameClaim> {
    self
      .local
      .keys()
      .filter_map(|name| self.claims.get(name).cloned())
      .collect()
  }
}

/// Handle to the cluster-wide name registry of one [`Mesh`] (see [`Mesh::global`]).
#[derive(Clone)]
pub struct GlobalRegistry {
  mesh: Mesh,
}

impl GlobalRegistry {
  pub(crate) fn new(mesh: Mesh) -> Self {
    Self { mesh }
  }

  /// Register the local process `pid` under `name`, cluster-wide.
  ///
  /// Fails with [`GlobalNameError::AlreadyRegistered`] when this node's replica already holds
  /// the name. The name is released when the process exits, when it is unregistered, or when
  /// it loses a conflict (see the module docs).
  pub fn register(&self, name: impl Into<String>, pid: &Pid) -> Result<(), GlobalNameError> {
    self.claim(name.into(), pid, None)
  }

  /// [`GlobalRegistry::register`] for a typed address, so that
  /// [`GlobalRegistry::whereis_addr`] can hand the [`Addr`] back on this node.
  pub fn register_addr<P: Process>(
    &self,
    name: impl Into<String>,
    addr: &Addr<P>,
  ) -> Result<(), GlobalNameError> {
    self.claim(name.into(), addr.pid(), Some(Box::new(addr.clone())))
  }

  fn claim(
    &self,
    name: String,
    pid: &Pid,
    addr: Option<Box<dyn Any + Send + Sync>>,
  ) -> Result<(), GlobalNameError> {
    if !valid_name(&name) {
      return Err(GlobalNameError::InvalidName);
    }
    if pid.node().as_str() != self.mesh.node_name() {
      return Err(GlobalNameError::NotLocal(pid.clone()));
    }
    let claim = NameClaim {
      name: name.clone(),
      pid: PidWire::from(pid),
      owner: self.mesh.node_id().clone(),
    };
    {
      let mut names = self.mesh.global_names().lock().expect("global names");
      if let Some(held) = names.claims.get(&name) {
        return Err(GlobalNameError::AlreadyRegistered {
          name,
          owner: held.pid.to_pid(),
        });
      }
      names.claims.insert(name.clone(), claim.clone());
      names.local.insert(
        name.clone(),
        LocalClaim {
          pid: pid.clone(),
          monitor: None,
          addr,
        },
      );
    }
    // The monitor sink may fire right here (dead pid) or from the process's finalizer: it
    // releases the name, so no lock is held around it.
    let fired = Arc::new(AtomicBool::new(false));
    let sink: MonitorSink = {
      let (mesh, name, fired) = (self.mesh.clone(), name.clone(), Arc::clone(&fired));
      Box::new(move |_monitor, dead, _reason| {
        fired.store(true, Ordering::SeqCst);
        release_local(&mesh, &name, &dead);
      })
    };
    let monitor = self.mesh.process().monitor_with_sink(pid, sink);
    if fired.load(Ordering::SeqCst) {
      return Err(GlobalNameError::NoProc(pid.clone()));
    }
    if let Some(local) = self
      .mesh
      .global_names()
      .lock()
      .expect("global names")
      .local
      .get_mut(&name)
      .filter(|local| &local.pid == pid)
    {
      local.monitor = Some(monitor);
    }
    self
      .mesh
      .broadcast_to_peers(ProcessEnvelope::GlobalRegister { claim });
    // Exited while the claim was on its way out: make sure peers see the release after it.
    if !self.is_local_holder(&name, pid) && fired.load(Ordering::SeqCst) {
      self
        .mesh
        .broadcast_to_peers(ProcessEnvelope::GlobalUnregister {
          name,
          pid: PidWire::from(pid),
        });
    }
    Ok(())
  }

  fn is_local_holder(&self, name: &str, pid: &Pid) -> bool {
    self
      .mesh
      .global_names()
      .lock()
      .expect("global names")
      .local
      .get(name)
      .is_some_and(|local| &local.pid == pid)
  }

  /// Release `name`. `Ok(true)` when this node held it, `Ok(false)` when nobody does, and
  /// [`GlobalNameError::NotLocal`] when another node owns it (only the owner releases a name).
  pub fn unregister(&self, name: &str) -> Result<bool, GlobalNameError> {
    let local = {
      let mut names = self.mesh.global_names().lock().expect("global names");
      match names.local.remove(name) {
        Some(local) => {
          names.claims.remove(name);
          local
        }
        None => {
          return match names.claims.get(name) {
            Some(held) => Err(GlobalNameError::NotLocal(held.pid.to_pid())),
            None => Ok(false),
          };
        }
      }
    };
    if let Some(monitor) = local.monitor {
      self.mesh.process().demonitor(&local.pid, monitor);
    }
    self
      .mesh
      .broadcast_to_peers(ProcessEnvelope::GlobalUnregister {
        name: name.to_string(),
        pid: PidWire::from(&local.pid),
      });
    Ok(true)
  }

  /// The process registered under `name`, wherever it runs.
  pub fn whereis(&self, name: &str) -> Option<Pid> {
    self
      .mesh
      .global_names()
      .lock()
      .expect("global names")
      .claims
      .get(name)
      .map(|claim| claim.pid.to_pid())
  }

  /// The typed address registered under `name` with [`GlobalRegistry::register_addr`], when
  /// the process runs on this node and is a `P`. Remote holders are reached by pid through
  /// [`GlobalRegistry::cast`] / [`GlobalRegistry::call`] (or [`Mesh::remote_call`]).
  pub fn whereis_addr<P: Process>(&self, name: &str) -> Option<Addr<P>> {
    self
      .mesh
      .global_names()
      .lock()
      .expect("global names")
      .local
      .get(name)
      .and_then(|local| local.addr.as_ref())
      .and_then(|addr| addr.downcast_ref::<Addr<P>>().cloned())
  }

  /// Every registered name and its process, sorted by name.
  pub fn names(&self) -> Vec<(String, Pid)> {
    self
      .mesh
      .global_names()
      .lock()
      .expect("global names")
      .claims
      .values()
      .map(|claim| (claim.name.clone(), claim.pid.to_pid()))
      .collect()
  }

  /// Cast postcard-encoded `msg` to the process registered under `name`
  /// ([`RemoteFault::NoProc`] when the name is free).
  pub fn cast(&self, name: &str, msg: Vec<u8>) -> Result<(), RemoteFault> {
    match self.whereis(name) {
      Some(pid) => self.mesh.remote_cast(&pid, msg),
      None => Err(RemoteFault::NoProc),
    }
  }

  /// Call the process registered under `name`, resolved when the effect runs
  /// ([`RemoteFault::NoProc`] when the name is free; otherwise as [`Mesh::remote_call`]).
  pub fn call<R: Send + Sync + 'static>(
    &self,
    name: &str,
    msg: Vec<u8>,
    timeout: Duration,
  ) -> Effect<Vec<u8>, RemoteFault, R> {
    let registry = self.clone();
    let name = name.to_string();
    suspend(move || match registry.whereis(&name) {
      Some(pid) => registry.mesh.remote_call(&pid, msg, timeout),
      None => fail(RemoteFault::NoProc),
    })
  }

  /// Send every claim this node owns to `node`, which replaces what it held for this node and
  /// resolves conflicts. Call it on both sides when a partition heals ([`Mesh::add_peer`] does
  /// it for a new peer).
  pub fn sync_with(&self, node: &str) -> Result<(), RemoteFault> {
    let claims = self
      .mesh
      .global_names()
      .lock()
      .expect("global names")
      .owned_claims();
    self
      .mesh
      .send_envelope(node, ProcessEnvelope::GlobalSync { claims })
  }
}

/// The process holding a local claim exited: forget the claim and tell the peers.
fn release_local(mesh: &Mesh, name: &str, pid: &Pid) {
  let wire = PidWire::from(pid);
  let released = {
    let mut names = mesh.global_names().lock().expect("global names");
    if names.local.get(name).is_some_and(|local| &local.pid == pid) {
      names.local.remove(name);
      if names
        .claims
        .get(name)
        .is_some_and(|claim| claim.pid == wire)
      {
        names.claims.remove(name);
      }
      true
    } else {
      false
    }
  };
  if released {
    mesh.broadcast_to_peers(ProcessEnvelope::GlobalUnregister {
      name: name.to_string(),
      pid: wire,
    });
  }
}

/// What merging one inbound claim requires once the lock is released.
enum Merged {
  Nothing,
  /// A local claim lost: signal its process.
  Lost(String, LocalClaim),
  /// The inbound claim lost to one this node owns: tell its sender.
  Answer(NameClaim),
}

fn merge(mesh: &Mesh, claim: NameClaim) -> Merged {
  let mut names = mesh.global_names().lock().expect("global names");
  if let Some(held) = names.claims.get(&claim.name) {
    if *held == claim {
      return Merged::Nothing;
    }
    if !claim.wins_over(held) {
      return if held.owner == *mesh.node_id() {
        Merged::Answer(held.clone())
      } else {
        Merged::Nothing
      };
    }
  }
  let name = claim.name.clone();
  let displaced = names.claims.insert(name.clone(), claim);
  match displaced {
    Some(lost) if lost.owner == *mesh.node_id() => names
      .local
      .remove(&name)
      .map_or(Merged::Nothing, |local| Merged::Lost(name, local)),
    _ => Merged::Nothing,
  }
}

fn settle(mesh: &Mesh, origin: Option<&NodeId>, merged: Merged) {
  match merged {
    Merged::Nothing => {}
    Merged::Lost(name, local) => {
      if let Some(monitor) = local.monitor {
        mesh.process().demonitor(&local.pid, monitor);
      }
      mesh.process().exit(&local.pid, name_conflict(&name));
    }
    Merged::Answer(winner) => {
      if let Some(origin) = origin {
        let _ = mesh.send_envelope(
          origin.name(),
          ProcessEnvelope::GlobalRegister { claim: winner },
        );
      }
    }
  }
}

/// Whether `claim` may come from `origin`: well formed, owned by the sender (when known), and
/// never about this node (which is the only authority on its own names).
fn admissible(mesh: &Mesh, origin: Option<&NodeId>, claim: &NameClaim) -> bool {
  claim.well_formed()
    && claim.owner.name() != mesh.node_name()
    && origin.is_none_or(|o| o.same_node(&claim.owner))
}

/// Inbound [`ProcessEnvelope::GlobalRegister`].
pub(crate) fn on_register(mesh: &Mesh, origin: Option<&NodeId>, claim: NameClaim) {
  if admissible(mesh, origin, &claim) {
    let merged = merge(mesh, claim);
    settle(mesh, origin, merged);
  }
}

/// Inbound [`ProcessEnvelope::GlobalUnregister`].
pub(crate) fn on_unregister(mesh: &Mesh, origin: Option<&NodeId>, name: &str, pid: &PidWire) {
  let mut names = mesh.global_names().lock().expect("global names");
  let held_by_sender = names.claims.get(name).is_some_and(|claim| {
    claim.pid == *pid
      && claim.owner.name() != mesh.node_name()
      && origin.is_none_or(|o| o.same_node(&claim.owner))
  });
  if held_by_sender {
    names.claims.remove(name);
  }
}

/// Inbound [`ProcessEnvelope::GlobalSync`]: the sender's complete set of claims.
pub(crate) fn on_sync(mesh: &Mesh, origin: Option<&NodeId>, claims: Vec<NameClaim>) {
  let claims: Vec<NameClaim> = claims
    .into_iter()
    .filter(|claim| admissible(mesh, origin, claim))
    .collect();
  if let Some(origin) = origin {
    // Anything held for the sender that it no longer claims was released while we were apart.
    let mut names = mesh.global_names().lock().expect("global names");
    names
      .claims
      .retain(|_, held| !held.owner.same_node(origin) || claims.iter().any(|claim| claim == held));
  }
  for claim in claims {
    let merged = merge(mesh, claim);
    settle(mesh, origin, merged);
  }
}

/// A peer node went down: every claim it owned is released.
pub(crate) fn node_down(mesh: &Mesh, peer_name: &str) {
  mesh
    .global_names()
    .lock()
    .expect("global names")
    .claims
    .retain(|_, claim| claim.owner.name() != peer_name);
}

/// A peer connected: send it the claims this node owns (nothing when it owns none).
pub(crate) fn peer_up(mesh: &Mesh, peer_name: &str) {
  let claims = mesh
    .global_names()
    .lock()
    .expect("global names")
    .owned_claims();
  if !claims.is_empty() {
    let _ = mesh.send_envelope(peer_name, ProcessEnvelope::GlobalSync { claims });
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn claim(name: &str, node: &str, incarnation: u128, id: u64) -> NameClaim {
    NameClaim {
      name: name.to_string(),
      pid: PidWire {
        node: node.to_string(),
        id,
        incarnation: 1,
      },
      owner: NodeId::from_parts(node, incarnation),
    }
  }

  #[test]
  fn lowest_incarnation_wins_whatever_the_order() {
    let old = claim("svc", "b@test", 1, 9);
    let young = claim("svc", "a@test", 2, 1);
    assert!(old.wins_over(&young));
    assert!(!young.wins_over(&old));
  }

  #[test]
  fn equal_incarnations_fall_back_to_node_name() {
    let a = claim("svc", "a@test", 7, 5);
    let b = claim("svc", "b@test", 7, 1);
    assert!(a.wins_over(&b));
    assert!(!b.wins_over(&a));
  }

  #[test]
  fn the_owner_node_replaces_its_own_claim() {
    let first = claim("svc", "a@test", 1, 1);
    let second = claim("svc", "a@test", 1, 2);
    let reborn = claim("svc", "a@test", 5, 1);
    assert!(second.wins_over(&first));
    assert!(first.wins_over(&second));
    assert!(reborn.wins_over(&first));
  }

  #[test]
  fn claims_must_name_a_pid_on_the_owner() {
    assert!(claim("svc", "a@test", 1, 1).well_formed());
    let mut forged = claim("svc", "a@test", 1, 1);
    forged.pid.node = "b@test".to_string();
    assert!(!forged.well_formed());
    assert!(!claim("", "a@test", 1, 1).well_formed());
    assert!(!claim(&"x".repeat(MAX_NAME_LEN + 1), "a@test", 1, 1).well_formed());
  }

  #[test]
  fn name_conflict_reason_round_trips() {
    let reason = name_conflict("svc");
    assert_eq!(conflicting_name(&reason), Some("svc"));
    assert_eq!(conflicting_name(&ExitReason::Killed), None);
    assert_eq!(
      conflicting_name(&ExitReason::Error("boom".to_string())),
      None
    );
  }

  #[test]
  fn claims_survive_serde() {
    let c = claim("svc", "a@test", u128::MAX, 3);
    let bytes = postcard::to_allocvec(&c).expect("encode");
    let back: NameClaim = postcard::from_bytes(&bytes).expect("decode");
    assert_eq!(back, c);
  }
}
