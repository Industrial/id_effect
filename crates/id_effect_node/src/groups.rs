//! AP process groups as an ORSWOT CRDT (ADR 0009).
//!
//! Netsplit: duplicates/gaps possible during partition; merge on heal is
//! associative, commutative, and idempotent.

use std::collections::{BTreeMap, BTreeSet};

use id_effect_process::Pid;
use serde::{Deserialize, Serialize};

use crate::envelope::PidWire;

/// Unique tag for an add (actor + counter).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Dot {
  /// Actor that issued the add.
  pub actor: String,
  /// Monotonic counter for that actor.
  pub counter: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
  pid: PidWire,
  dot: Dot,
}

/// Observed-Remove Set without tombstones (ORSWOT).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrSwot {
  entries: BTreeMap<String, Entry>,
  vv: BTreeMap<String, u64>,
}

impl OrSwot {
  /// Empty set.
  pub fn new() -> Self {
    Self::default()
  }

  fn key(pid: &PidWire) -> String {
    format!("{}:{}:{}", pid.node, pid.id, pid.incarnation)
  }

  /// Add pid under actor.
  pub fn add(&mut self, actor: &str, pid: &Pid) {
    let counter = self.vv.entry(actor.to_string()).or_insert(0);
    *counter += 1;
    let dot = Dot {
      actor: actor.to_string(),
      counter: *counter,
    };
    let wire = PidWire::from(pid);
    self
      .entries
      .insert(Self::key(&wire), Entry { pid: wire, dot });
  }

  /// Remove pid if present.
  pub fn remove(&mut self, pid: &Pid) {
    let wire = PidWire::from(pid);
    self.entries.remove(&Self::key(&wire));
  }

  /// Merge another replica (join).
  ///
  /// Uses each replica's *pre-merge* version vector to decide whether a
  /// unilateral remove observed the absent add (classic ORSWOT).
  pub fn merge(&mut self, other: &OrSwot) {
    let self_vv = self.vv.clone();
    let mut keep: BTreeMap<String, Entry> = BTreeMap::new();
    let keys: BTreeSet<_> = self
      .entries
      .keys()
      .chain(other.entries.keys())
      .cloned()
      .collect();
    for k in keys {
      let in_self = self.entries.get(&k);
      let in_other = other.entries.get(&k);
      match (in_self, in_other) {
        (Some(a), Some(b)) => {
          let win = if a.dot >= b.dot { a } else { b };
          keep.insert(k, win.clone());
        }
        (Some(a), None) => {
          let seen = other.vv.get(&a.dot.actor).copied().unwrap_or(0);
          if seen < a.dot.counter {
            keep.insert(k, a.clone());
          }
        }
        (None, Some(b)) => {
          let seen = self_vv.get(&b.dot.actor).copied().unwrap_or(0);
          if seen < b.dot.counter {
            keep.insert(k, b.clone());
          }
        }
        (None, None) => {}
      }
    }
    self.entries = keep;
    for (actor, &cnt) in &other.vv {
      let e = self.vv.entry(actor.clone()).or_insert(0);
      *e = (*e).max(cnt);
    }
  }

  /// Members as pids.
  pub fn members(&self) -> Vec<Pid> {
    self.entries.values().map(|e| e.pid.to_pid()).collect()
  }

  /// Member count.
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  /// Empty?
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }
}

/// Named process group store.
#[derive(Clone, Debug, Default)]
pub struct ProcessGroups {
  groups: BTreeMap<String, OrSwot>,
  actor: String,
}

impl ProcessGroups {
  /// Local replica owned by actor (usually node name).
  pub fn new(actor: impl Into<String>) -> Self {
    Self {
      groups: BTreeMap::new(),
      actor: actor.into(),
    }
  }

  /// Join pid to group.
  pub fn join(&mut self, group: &str, pid: &Pid) {
    self
      .groups
      .entry(group.to_string())
      .or_default()
      .add(&self.actor, pid);
  }

  /// Leave group.
  pub fn leave(&mut self, group: &str, pid: &Pid) {
    if let Some(g) = self.groups.get_mut(group) {
      g.remove(pid);
    }
  }

  /// Members of a group.
  pub fn members(&self, group: &str) -> Vec<Pid> {
    self
      .groups
      .get(group)
      .map(|g| g.members())
      .unwrap_or_default()
  }

  /// Merge remote replica state for one group.
  pub fn merge_group(&mut self, group: &str, remote: &OrSwot) {
    self
      .groups
      .entry(group.to_string())
      .or_default()
      .merge(remote);
  }

  /// Snapshot one group CRDT for gossip.
  pub fn snapshot(&self, group: &str) -> OrSwot {
    self.groups.get(group).cloned().unwrap_or_default()
  }

  /// All group names.
  pub fn group_names(&self) -> BTreeSet<String> {
    self.groups.keys().cloned().collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use id_effect_process::NodeName;

  fn pid(node: &str, id: u64) -> Pid {
    Pid::from_parts(NodeName::new(node), id, 1)
  }

  #[test]
  fn merge_is_idempotent_commutative() {
    let mut a = OrSwot::new();
    let mut b = OrSwot::new();
    a.add("a", &pid("n1", 1));
    b.add("b", &pid("n2", 2));

    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);
    assert_eq!(ab, ba, "commutative");

    let mut ab2 = ab.clone();
    ab2.merge(&b);
    assert_eq!(ab, ab2, "idempotent");
    assert_eq!(ab.len(), 2);
  }

  #[test]
  fn remove_dominates_stale_add_after_merge() {
    let mut a = OrSwot::new();
    let p = pid("n1", 1);
    a.add("a", &p);
    let mut b = a.clone();
    a.remove(&p);
    b.merge(&a);
    assert!(b.is_empty(), "remove wins on merge");
  }

  #[test]
  fn concurrent_re_add_wins_over_remove() {
    let p = pid("n1", 1);
    let mut a = OrSwot::new();
    a.add("a", &p);
    let mut b = a.clone();
    a.remove(&p);
    // b re-adds concurrently with a fresh dot a has never observed.
    b.add("b", &p);
    let mut merged = a.clone();
    merged.merge(&b);
    assert_eq!(merged.members(), vec![p.clone()], "add-wins");
    b.merge(&a);
    assert_eq!(b, merged, "both replicas converge");
  }

  #[test]
  fn merge_is_associative() {
    let (mut a, mut b, mut c) = (OrSwot::new(), OrSwot::new(), OrSwot::new());
    a.add("a", &pid("n1", 1));
    b.add("b", &pid("n2", 2));
    c.add("c", &pid("n3", 3));
    c.remove(&pid("n3", 3));
    c.add("c", &pid("n4", 4));

    let mut left = a.clone();
    left.merge(&b);
    left.merge(&c);
    let mut bc = b.clone();
    bc.merge(&c);
    let mut right = a.clone();
    right.merge(&bc);
    assert_eq!(left, right);
  }

  #[test]
  fn re_adding_the_same_pid_does_not_duplicate() {
    let mut set = OrSwot::new();
    set.add("a", &pid("n1", 1));
    set.add("a", &pid("n1", 1));
    assert_eq!(set.len(), 1);
  }

  #[test]
  fn same_id_different_incarnation_are_distinct_members() {
    let mut set = OrSwot::new();
    set.add("a", &pid("n1", 1));
    set.add("a", &Pid::from_parts(NodeName::new("n1"), 1, 2));
    assert_eq!(set.len(), 2);
  }

  #[test]
  fn unknown_groups_are_empty_and_leave_is_a_noop() {
    let mut groups = ProcessGroups::new("a");
    groups.leave("ghost", &pid("n1", 1));
    assert!(groups.members("ghost").is_empty());
    assert!(groups.snapshot("ghost").is_empty());
    assert!(groups.group_names().is_empty());
  }

  #[test]
  fn merge_group_creates_the_group_locally() {
    let mut remote = ProcessGroups::new("b");
    remote.join("g", &pid("n2", 2));
    let mut local = ProcessGroups::new("a");
    local.merge_group("g", &remote.snapshot("g"));
    assert_eq!(local.members("g"), vec![pid("n2", 2)]);
    assert!(local.group_names().contains("g"));
  }

  #[test]
  fn crdt_state_survives_serde() {
    let mut set = OrSwot::new();
    set.add("a", &pid("n1", 1));
    let bytes = postcard::to_allocvec(&set).expect("encode");
    let back: OrSwot = postcard::from_bytes(&bytes).expect("decode");
    assert_eq!(back, set);
  }

  proptest::proptest! {
    #[test]
    fn merge_converges_for_any_op_history(
      ops_a in proptest::collection::vec((0u64..6, proptest::prelude::any::<bool>()), 0..12),
      ops_b in proptest::collection::vec((0u64..6, proptest::prelude::any::<bool>()), 0..12),
    ) {
      let apply = |actor: &str, ops: &[(u64, bool)]| {
        let mut set = OrSwot::new();
        for &(id, add) in ops {
          if add {
            set.add(actor, &pid("n", id));
          } else {
            set.remove(&pid("n", id));
          }
        }
        set
      };
      let (a, b) = (apply("a", &ops_a), apply("b", &ops_b));
      let mut ab = a.clone();
      ab.merge(&b);
      let mut ba = b.clone();
      ba.merge(&a);
      proptest::prop_assert_eq!(&ab, &ba);
      let mut again = ab.clone();
      again.merge(&b);
      proptest::prop_assert_eq!(&again, &ab);
    }
  }
}
