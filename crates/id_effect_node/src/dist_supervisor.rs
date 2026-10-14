//! [`DistributedSupervisor`] — Horde-style takeover on NodeDown.
//!
//! Children are placed on mesh nodes; when a hosting node departs,
//! surviving supervisors re-spawn the child via the behavior registry
//! (ADR 0009 § distributed supervision). A child whose takeover fails is
//! kept as *orphaned* and retried — off the membership path — when a node
//! joins, or on demand via [`DistributedSupervisor::retry_orphaned`]; it is
//! never dropped.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use id_effect::ResourcePolicy;
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

/// Handle to an orphan retry running off the membership path. Joining it
/// yields the children that are still orphaned, with their latest failure.
pub type RetryHandle = std::thread::JoinHandle<Vec<(String, RemoteFault)>>;

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
  pub fn start_child(
    &self,
    spec: DistChildSpec,
    node_name: Option<&str>,
  ) -> Result<Pid, RemoteFault> {
    match node_name {
      Some(node) => {
        let pid = self.inner.mesh.remote_spawn(
          node,
          &spec.behavior,
          spec.args.clone(),
          None,
          SPAWN_TIMEOUT,
        )?;
        self.inner.insert_running(spec, pid.clone());
        Ok(pid)
      }
      None => self.inner.place_and_run(spec, None),
    }
  }

  /// React to membership.
  ///
  /// `NodeDown` re-places the departed node's children synchronously; a
  /// child no node can host stays tracked as orphaned (see
  /// [`DistributedSupervisor::orphaned`]) and a warning is logged.
  /// `NodeUp` / `Rejoined` retry orphaned children on a background thread
  /// so membership handling never blocks on spawn timeouts; the handle is
  /// returned when a retry was started.
  pub fn on_membership(&self, event: &MembershipEvent) -> Option<RetryHandle> {
    match event {
      MembershipEvent::NodeDown(member) => {
        self.inner.take_over(member.id.name());
        None
      }
      MembershipEvent::NodeUp(member) | MembershipEvent::Rejoined(member) => {
        if self.inner.orphans.lock().expect("orphans").is_empty() {
          return None;
        }
        let inner = Arc::clone(&self.inner);
        let joined = member.id.name().to_string();
        std::thread::Builder::new()
          .name("dist-supervisor-retry".into())
          .spawn(move || inner.retry(Some(&joined)))
          .ok()
      }
    }
  }

  /// Try to place every orphaned child again (synchronously), avoiding the
  /// node each one was orphaned by. Children that start move back to the
  /// running set; the rest stay orphaned and are returned with the failure
  /// from this attempt. Children a concurrent retry owns are skipped.
  pub fn retry_orphaned(&self) -> Vec<(String, RemoteFault)> {
    self.inner.retry(None)
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
  fn place_and_run(&self, spec: DistChildSpec, exclude: Option<&str>) -> Result<Pid, RemoteFault> {
    let reports: Vec<LoadReport> = self
      .mesh
      .load_reports()
      .into_iter()
      .filter(|r| Some(r.node.as_str()) != exclude)
      .collect();
    let prefer = spec.prefer.as_deref().filter(|p| Some(*p) != exclude);

    let mut candidates: Vec<String> = Vec::new();
    if let Some(pref) = prefer {
      let mut biased = self.policy.clone();
      biased.placement = PlacementMode::Affinity {
        node: pref.to_string(),
      };
      candidates.extend(place(&reports, &biased));
      candidates.push(pref.to_string());
    }
    candidates.extend(place(&reports, &self.policy));
    candidates.push(self.mesh.node_name().to_string());

    let mut last = RemoteFault::NoProc;
    let mut tried: Vec<&str> = Vec::new();
    for target in &candidates {
      if Some(target.as_str()) == exclude || tried.contains(&target.as_str()) {
        continue;
      }
      tried.push(target);
      match self.mesh.remote_spawn(
        target,
        &spec.behavior,
        spec.args.clone(),
        None,
        SPAWN_TIMEOUT,
      ) {
        Ok(pid) => {
          self.insert_running(spec, pid.clone());
          return Ok(pid);
        }
        Err(fault) => last = fault,
      }
    }
    Err(last)
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

  fn take_over(&self, down: &str) {
    let survivors: Vec<DistChildSpec> = {
      let mut kids = self.children.lock().expect("children");
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
    for spec in survivors {
      // Re-place over survivors, excluding the downed node.
      if let Err(fault) = self.place_and_run(spec.clone(), Some(down)) {
        self.orphan(spec, fault, down.to_string());
      }
    }
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

  /// Retry every unclaimed orphan. `joined` is the node whose arrival
  /// triggered the retry: an orphan's departed node is avoided unless it is
  /// the one that just (re)joined.
  fn retry(&self, joined: Option<&str>) -> Vec<(String, RemoteFault)> {
    let claimed: Vec<(DistChildSpec, String)> = self
      .orphans
      .lock()
      .expect("orphans")
      .values_mut()
      .filter(|o| !o.retrying)
      .map(|o| {
        o.retrying = true;
        (o.spec.clone(), o.departed.clone())
      })
      .collect();
    let mut failed = Vec::new();
    for (spec, departed) in claimed {
      let exclude = (Some(departed.as_str()) != joined).then_some(departed.as_str());
      if let Err(fault) = self.place_and_run(spec.clone(), exclude) {
        failed.push((spec.id.clone(), fault.clone()));
        self.orphan(spec, fault, departed);
      }
    }
    failed
  }
}
