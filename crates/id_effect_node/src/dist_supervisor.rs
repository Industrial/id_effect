//! [`DistributedSupervisor`] — Horde-style takeover on NodeDown.
//!
//! Children are placed on mesh nodes; when a hosting node departs,
//! surviving supervisors re-spawn the child via the behavior registry
//! (ADR 0009 § distributed supervision). A child whose takeover fails is
//! kept as *orphaned* and retried — off the membership path, on a daemon
//! fiber — when a node joins, or on demand via
//! [`DistributedSupervisor::retry_orphaned`]; it is never dropped.
//!
//! Every operation that spawns is an effect (`Send`, interruptible): placement tries its
//! candidates one after another, each remote spawn bounded by a timeout on the executor clock.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::Never;
use id_effect::compute::ResourcePolicy;
use id_effect::{Effect, FiberHandle, fail, succeed, suspend, sync};
use id_effect_process::Pid;

use crate::envelope::RemoteFault;
use crate::membership::MembershipEvent;
use crate::mesh::Mesh;
use crate::placement::{ClusterResourcePolicy, LoadReport, PlacementMode, place};

/// How long to wait for a remote spawn during placement/takeover.
const SPAWN_TIMEOUT: Duration = Duration::from_secs(5);

/// A child managed across the cluster.
#[derive(Clone, Debug)]
pub struct DistChildSpec {
  /// Stable child id within the supervisor.
  pub id: String,
  /// Registered behavior name.
  pub behavior: String,
  /// Postcard-encoded init args.
  pub args: Vec<u8>,
  /// Preferred node name; `None` = any healthy member.
  pub prefer: Option<String>,
}

/// Handle to an orphan retry running off the membership path (a daemon fiber). Joining it
/// yields the children that are still orphaned, with their latest failure; interrupting it
/// stops the retry (its unfinished children stay orphaned and retryable).
pub type RetryHandle = FiberHandle<Vec<(String, RemoteFault)>, Never>;

#[derive(Clone, Debug)]
struct Running {
  spec: DistChildSpec,
  pid: Pid,
  node: String,
}

#[derive(Clone, Debug)]
struct Orphan {
  spec: DistChildSpec,
  fault: RemoteFault,
  /// The node whose departure orphaned the child; retries avoid it until
  /// it rejoins.
  departed: String,
  /// A retry currently owns this child (prevents duplicate spawns).
  retrying: bool,
}

struct Inner {
  mesh: Arc<Mesh>,
  policy: ClusterResourcePolicy,
  children: Mutex<HashMap<String, Running>>,
  orphans: Mutex<HashMap<String, Orphan>>,
}

/// Distributed supervisor state for one logical tree node.
pub struct DistributedSupervisor {
  inner: Arc<Inner>,
}

impl DistributedSupervisor {
  /// Bind to a mesh with a permissive local-first placement policy.
  ///
  /// The default admits any node below 100% CPU and memory and prefers the
  /// least-loaded one; override with [`DistributedSupervisor::with_policy`] to
  /// enforce tighter caps or a different [`PlacementMode`].
  pub fn new(mesh: Arc<Mesh>) -> Self {
    Self::with_policy(
      mesh,
      ClusterResourcePolicy::local_first(ResourcePolicy::memory_cap_max_cpu(1.0)),
    )
  }

  /// Bind to a mesh with an explicit cluster placement policy.
  pub fn with_policy(mesh: Arc<Mesh>, policy: ClusterResourcePolicy) -> Self {
    Self {
      inner: Arc::new(Inner {
        mesh,
        policy,
        children: Mutex::new(HashMap::new()),
        orphans: Mutex::new(HashMap::new()),
      }),
    }
  }

  /// Start (or restart) a child.
  ///
  /// With `node_name` set, the child is pinned to that node. Otherwise the
  /// candidates are tried in order until one spawns the child: the node
  /// chosen by [`crate::placement::place`] with `prefer` as an
  /// [`PlacementMode::Affinity`] bias, then `prefer` itself, then plain
  /// placement over the mesh's load telemetry, then the local node.
  pub fn start_child<R: Send + Sync + 'static>(
    &self,
    spec: DistChildSpec,
    node_name: Option<&str>,
  ) -> Effect<Pid, RemoteFault, R> {
    let inner = Arc::clone(&self.inner);
    match node_name {
      Some(node) => inner
        .mesh
        .remote_spawn(node, &spec.behavior, spec.args.clone(), None, SPAWN_TIMEOUT)
        .flat_map(move |pid| {
          sync(move || {
            inner.insert_running(spec, pid.clone());
            pid
          })
        }),
      None => Inner::place_and_run(inner, spec, None),
    }
  }

  /// React to membership.
  ///
  /// `NodeDown` re-places the departed node's children (the effect completes once they are
  /// re-placed); a child no node can host stays tracked as orphaned (see
  /// [`DistributedSupervisor::orphaned`]) and a warning is logged.
  /// `NodeUp` / `Rejoined` retry orphaned children on a daemon fiber so membership handling
  /// never waits on spawn timeouts; its handle is returned when a retry was started.
  pub fn on_membership<R: Send + Sync + 'static>(
    &self,
    event: &MembershipEvent,
  ) -> Effect<Option<RetryHandle>, Never, R> {
    let inner = Arc::clone(&self.inner);
    match event {
      MembershipEvent::NodeDown(member) => {
        Inner::take_over(inner, member.id.name().to_string()).map(|()| None)
      }
      MembershipEvent::NodeUp(member) | MembershipEvent::Rejoined(member) => {
        let joined = member.id.name().to_string();
        suspend(move || {
          if inner.orphans.lock().expect("orphans").is_empty() {
            return succeed(None);
          }
          Inner::retry::<R>(inner, Some(joined))
            .fork_daemon()
            .map(Some)
        })
      }
    }
  }

  /// Try to place every orphaned child again, avoiding the node each one was orphaned by.
  /// Children that start move back to the running set; the rest stay orphaned and are
  /// returned with the failure from this attempt. Children a concurrent retry owns are
  /// skipped.
  pub fn retry_orphaned<R: Send + Sync + 'static>(
    &self,
  ) -> Effect<Vec<(String, RemoteFault)>, Never, R> {
    Inner::retry(Arc::clone(&self.inner), None)
  }

  /// Orphaned children (host lost, not yet re-placed) with their latest
  /// placement failure. They are not counted by [`DistributedSupervisor::len`].
  pub fn orphaned(&self) -> Vec<(String, RemoteFault)> {
    self
      .inner
      .orphans
      .lock()
      .expect("orphans")
      .iter()
      .map(|(id, o)| (id.clone(), o.fault.clone()))
      .collect()
  }

  /// Running child count (orphaned children are excluded).
  pub fn len(&self) -> usize {
    self.inner.children.lock().expect("children").len()
  }

  /// Whether no child is running (orphaned children are excluded).
  pub fn is_empty(&self) -> bool {
    self.inner.children.lock().expect("children").is_empty()
  }

  /// Snapshot of (child id, pid, node).
  pub fn list(&self) -> Vec<(String, Pid, String)> {
    self
      .inner
      .children
      .lock()
      .expect("children")
      .iter()
      .map(|(id, r)| (id.clone(), r.pid.clone(), r.node.clone()))
      .collect()
  }
}

impl Inner {
  /// Spawn `spec` on the first candidate that accepts it (see
  /// [`DistributedSupervisor::start_child`] for the order), never on
  /// `exclude` (e.g. a downed node).
  fn place_and_run<R: Send + Sync + 'static>(
    this: Arc<Self>,
    spec: DistChildSpec,
    exclude: Option<String>,
  ) -> Effect<Pid, RemoteFault, R> {
    suspend(move || {
      let exclude = exclude.as_deref();
      let reports: Vec<LoadReport> = this
        .mesh
        .load_reports()
        .into_iter()
        .filter(|r| Some(r.node.as_str()) != exclude)
        .collect();
      let prefer = spec.prefer.as_deref().filter(|p| Some(*p) != exclude);

      let mut candidates: Vec<String> = Vec::new();
      if let Some(pref) = prefer {
        let mut biased = this.policy.clone();
        biased.placement = PlacementMode::Affinity {
          node: pref.to_string(),
        };
        candidates.extend(place(&reports, &biased));
        candidates.push(pref.to_string());
      }
      candidates.extend(place(&reports, &this.policy));
      candidates.push(this.mesh.node_name().to_string());

      let mut order: Vec<String> = Vec::new();
      for target in candidates {
        if Some(target.as_str()) != exclude && !order.contains(&target) {
          order.push(target);
        }
      }
      order.reverse();
      Self::try_candidates(this, spec, order, RemoteFault::NoProc)
    })
  }

  /// Try the candidates (popped from the back) one at a time; fail with the last fault.
  fn try_candidates<R: Send + Sync + 'static>(
    this: Arc<Self>,
    spec: DistChildSpec,
    mut rest: Vec<String>,
    last: RemoteFault,
  ) -> Effect<Pid, RemoteFault, R> {
    let Some(target) = rest.pop() else {
      return fail(last);
    };
    let (on_ok, on_err) = (Arc::clone(&this), this);
    let (ok_spec, err_spec) = (spec.clone(), spec);
    on_ok
      .mesh
      .remote_spawn(
        &target,
        &ok_spec.behavior,
        ok_spec.args.clone(),
        None,
        SPAWN_TIMEOUT,
      )
      .flat_map(move |pid| {
        sync(move || {
          on_ok.insert_running(ok_spec, pid.clone());
          pid
        })
      })
      .catch(move |fault| Self::try_candidates(on_err, err_spec, rest, fault))
  }

  /// Record a running child keyed by spec id; node is taken from the live pid.
  fn insert_running(&self, spec: DistChildSpec, pid: Pid) {
    let node = pid.node().as_str().to_string();
    self.orphans.lock().expect("orphans").remove(&spec.id);
    self
      .children
      .lock()
      .expect("children")
      .insert(spec.id.clone(), Running { spec, pid, node });
  }

  fn take_over<R: Send + Sync + 'static>(this: Arc<Self>, down: String) -> Effect<(), Never, R> {
    suspend(move || {
      let survivors: Vec<DistChildSpec> = {
        let mut kids = this.children.lock().expect("children");
        let doomed: Vec<String> = kids
          .iter()
          .filter(|(_, r)| r.node == down || r.pid.node().as_str() == down)
          .map(|(id, _)| id.clone())
          .collect();
        doomed
          .into_iter()
          .filter_map(|id| kids.remove(&id).map(|r| r.spec))
          .collect()
      };
      survivors.into_iter().fold(succeed(()), |acc, spec| {
        let (this, down) = (Arc::clone(&this), down.clone());
        acc.flat_map(move |()| {
          // Re-place over survivors, excluding the downed node.
          Self::place_and_run(Arc::clone(&this), spec.clone(), Some(down.clone()))
            .map(|_| ())
            .catch(move |fault| sync(move || this.orphan(spec, fault, down)))
        })
      })
    })
  }

  fn orphan(&self, spec: DistChildSpec, fault: RemoteFault, departed: String) {
    tracing::warn!(child = %spec.id, %fault, "distributed child could not be placed; orphaned");
    self.orphans.lock().expect("orphans").insert(
      spec.id.clone(),
      Orphan {
        spec,
        fault,
        departed,
        retrying: false,
      },
    );
  }

  /// Hand claimed orphans back (a retry ended or was interrupted before finishing them).
  fn release_claims(&self, ids: &[String]) {
    let mut orphans = self.orphans.lock().expect("orphans");
    for id in ids {
      if let Some(o) = orphans.get_mut(id) {
        o.retrying = false;
      }
    }
  }

  /// Retry every unclaimed orphan. `joined` is the node whose arrival
  /// triggered the retry: an orphan's departed node is avoided unless it is
  /// the one that just (re)joined.
  fn retry<R: Send + Sync + 'static>(
    this: Arc<Self>,
    joined: Option<String>,
  ) -> Effect<Vec<(String, RemoteFault)>, Never, R> {
    // The claims are recorded inside the finalized region, so an interrupt at any point hands
    // every claimed orphan back.
    let claims: Arc<Mutex<Vec<String>>> = Arc::default();
    let (releaser, released) = (Arc::clone(&this), Arc::clone(&claims));
    suspend(move || {
      let claimed: Vec<(DistChildSpec, String)> = {
        let mut orphans = this.orphans.lock().expect("orphans");
        let mut ids = claims.lock().expect("claims");
        orphans
          .values_mut()
          .filter(|o| !o.retrying)
          .map(|o| {
            o.retrying = true;
            ids.push(o.spec.id.clone());
            (o.spec.clone(), o.departed.clone())
          })
          .collect()
      };
      claimed
        .into_iter()
        .fold(succeed(Vec::new()), |acc, (spec, departed)| {
          let this = Arc::clone(&this);
          let joined = joined.clone();
          acc.flat_map(move |mut failed: Vec<(String, RemoteFault)>| {
            let exclude = (Some(departed.as_str()) != joined.as_deref()).then(|| departed.clone());
            Self::place_and_run(Arc::clone(&this), spec.clone(), exclude)
              .map(|_| None)
              .catch(move |fault| {
                sync(move || {
                  this.orphan(spec.clone(), fault.clone(), departed);
                  Some((spec.id, fault))
                })
              })
              .map(move |f| {
                failed.extend(f);
                failed
              })
          })
        })
    })
    .ensuring(sync(move || {
      let ids = std::mem::take(&mut *released.lock().expect("claims"));
      releaser.release_claims(&ids);
    }))
  }
}
