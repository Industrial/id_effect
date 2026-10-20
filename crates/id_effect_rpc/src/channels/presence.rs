//! Presence: who is joined to a topic, with per-connection metadata, and the join/leave diffs
//! subscribers receive.
//!
//! One node's table holds the topics it has local subscribers on, nothing else: its own members
//! (alive as long as their subscription is) and the members other nodes announced for those
//! topics (alive until their node's heartbeat stops refreshing them or the node goes down). Each
//! topic holds at most the configured member cap, so a table is bounded by
//! `topics × members per topic`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One joined connection, as every subscriber of the topic sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Presence {
  /// Who joined: the principal's id (several connections may share a key).
  pub key: String,
  /// The joined connection; unique per node.
  pub conn: String,
  /// The node the connection is attached to.
  pub node: String,
  /// The metadata the connection joined with (`null` when none).
  pub meta: Value,
}

/// What changed on a topic: the connections that joined and those that left.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresenceDiff {
  /// Connections that joined.
  pub joins: Vec<Presence>,
  /// Connections that left (or whose node went away).
  pub leaves: Vec<Presence>,
}

impl PresenceDiff {
  /// Nothing changed.
  pub fn is_empty(&self) -> bool {
    self.joins.is_empty() && self.leaves.is_empty()
  }
}

/// A presence entry on the mesh (postcard): the metadata as JSON text, since postcard is not
/// self-describing; the node is the sending hub's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PresenceWire {
  pub(crate) key: String,
  pub(crate) conn: String,
  pub(crate) meta: String,
}

impl PresenceWire {
  pub(crate) fn of(presence: &Presence) -> Self {
    Self {
      key: presence.key.clone(),
      conn: presence.conn.clone(),
      meta: presence.meta.to_string(),
    }
  }

  /// The presence this entry announces for `node`; metadata that is not JSON reads as `null`.
  pub(crate) fn into_presence(self, node: &str) -> Presence {
    Presence {
      key: self.key,
      conn: self.conn,
      node: node.to_string(),
      meta: serde_json::from_str(&self.meta).unwrap_or(Value::Null),
    }
  }
}

#[derive(Clone, Debug)]
struct Entry {
  presence: Presence,
  /// When a remote entry lapses without a refresh; `None` for this node's own members.
  expires: Option<Duration>,
}

type Members = BTreeMap<(String, String), Entry>;

/// The presence of every topic this node has local subscribers on.
#[derive(Debug)]
pub(crate) struct PresenceTable {
  topics: BTreeMap<String, Members>,
  cap: usize,
}

impl PresenceTable {
  /// A table holding at most `cap` members per topic.
  pub(crate) fn new(cap: usize) -> Self {
    Self {
      topics: BTreeMap::new(),
      cap,
    }
  }

  /// Start tracking `topic` (its first local subscriber joined).
  pub(crate) fn track(&mut self, topic: &str) {
    self.topics.entry(topic.to_string()).or_default();
  }

  /// Stop tracking `topic` (its last local subscriber left): its members are forgotten.
  pub(crate) fn untrack(&mut self, topic: &str) {
    self.topics.remove(topic);
  }

  /// Members of `topic` in this node's view.
  pub(crate) fn count(&self, topic: &str) -> usize {
    self.topics.get(topic).map_or(0, BTreeMap::len)
  }

  /// Every member of `topic`, ordered by node then connection.
  pub(crate) fn list(&self, topic: &str) -> Vec<Presence> {
    self.topics.get(topic).map_or_else(Vec::new, |members| {
      members.values().map(|e| e.presence.clone()).collect()
    })
  }

  /// The members of `topic` attached to `node`.
  pub(crate) fn of_node(&self, topic: &str, node: &str) -> Vec<Presence> {
    self.topics.get(topic).map_or_else(Vec::new, |members| {
      members
        .values()
        .filter(|e| e.presence.node == node)
        .map(|e| e.presence.clone())
        .collect()
    })
  }

  /// Every node with a member on some topic.
  pub(crate) fn nodes(&self) -> BTreeSet<String> {
    self
      .topics
      .values()
      .flat_map(|members| members.keys().map(|(node, _)| node.clone()))
      .collect()
  }

  /// Add (or update) a member of a tracked topic. `false` when the topic is untracked or full
  /// and the member is new.
  pub(crate) fn insert(
    &mut self,
    topic: &str,
    presence: Presence,
    expires: Option<Duration>,
  ) -> bool {
    let Some(members) = self.topics.get_mut(topic) else {
      return false;
    };
    let key = (presence.node.clone(), presence.conn.clone());
    if !members.contains_key(&key) && members.len() >= self.cap {
      return false;
    }
    members.insert(key, Entry { presence, expires });
    true
  }

  /// Remove one member.
  pub(crate) fn remove(&mut self, topic: &str, node: &str, conn: &str) -> Option<Presence> {
    self
      .topics
      .get_mut(topic)?
      .remove(&(node.to_string(), conn.to_string()))
      .map(|e| e.presence)
  }

  /// Apply a diff `node` announced for `topic`; returns what changed here.
  pub(crate) fn apply(
    &mut self,
    topic: &str,
    node: &str,
    joins: Vec<Presence>,
    leaves: &[String],
    expires: Duration,
  ) -> PresenceDiff {
    let mut diff = PresenceDiff::default();
    for conn in leaves {
      if let Some(gone) = self.remove(topic, node, conn) {
        diff.leaves.push(gone);
      }
    }
    for presence in joins {
      let before = self.get(topic, node, &presence.conn);
      if self.insert(topic, presence.clone(), Some(expires)) && before.as_ref() != Some(&presence) {
        if let Some(old) = before {
          diff.leaves.push(old);
        }
        diff.joins.push(presence);
      }
    }
    diff
  }

  /// `node`'s complete member set for `topic` (a heartbeat): replaces what was held for it and
  /// refreshes the expiry of what stays; returns what changed here.
  pub(crate) fn replace_node(
    &mut self,
    topic: &str,
    node: &str,
    members: Vec<Presence>,
    expires: Duration,
  ) -> PresenceDiff {
    if !self.topics.contains_key(topic) {
      return PresenceDiff::default();
    }
    let held = self.of_node(topic, node);
    let leaves: Vec<String> = held
      .iter()
      .filter(|p| !members.iter().any(|m| m.conn == p.conn))
      .map(|p| p.conn.clone())
      .collect();
    let diff = self.apply(topic, node, members, &leaves, expires);
    if let Some(entries) = self.topics.get_mut(topic) {
      for entry in entries.values_mut().filter(|e| e.presence.node == node) {
        entry.expires = Some(expires);
      }
    }
    diff
  }

  /// Forget every member attached to `node` (it went down).
  pub(crate) fn drop_node(&mut self, node: &str) -> Vec<(String, PresenceDiff)> {
    self.drain(|e| e.presence.node == node)
  }

  /// Forget the remote members whose expiry is at or before `now`.
  pub(crate) fn expire(&mut self, now: Duration) -> Vec<(String, PresenceDiff)> {
    self.drain(|e| e.expires.is_some_and(|at| at <= now))
  }

  fn get(&self, topic: &str, node: &str, conn: &str) -> Option<Presence> {
    self
      .topics
      .get(topic)?
      .get(&(node.to_string(), conn.to_string()))
      .map(|e| e.presence.clone())
  }

  fn drain(&mut self, doomed: impl Fn(&Entry) -> bool) -> Vec<(String, PresenceDiff)> {
    let mut out = Vec::new();
    for (topic, members) in &mut self.topics {
      let gone: Vec<(String, String)> = members
        .iter()
        .filter(|(_, e)| doomed(e))
        .map(|(k, _)| k.clone())
        .collect();
      if gone.is_empty() {
        continue;
      }
      let leaves = gone
        .iter()
        .filter_map(|k| members.remove(k).map(|e| e.presence))
        .collect();
      out.push((
        topic.clone(),
        PresenceDiff {
          joins: Vec::new(),
          leaves,
        },
      ));
    }
    out
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use serde_json::json;

  fn p(node: &str, conn: &str) -> Presence {
    Presence {
      key: format!("user-{conn}"),
      conn: conn.to_string(),
      node: node.to_string(),
      meta: json!({"status": "online"}),
    }
  }

  const LATER: Duration = Duration::from_secs(30);

  #[test]
  fn untracked_topics_hold_nothing() {
    let mut table = PresenceTable::new(4);
    assert!(!table.insert("t", p("a", "1"), None));
    assert_eq!(table.count("t"), 0);
    assert!(table.list("t").is_empty());
    assert!(table.of_node("t", "a").is_empty());
    assert!(table.remove("t", "a", "1").is_none());
    assert!(
      table
        .replace_node("t", "b", vec![p("b", "2")], LATER)
        .is_empty()
    );
  }

  #[test]
  fn the_member_cap_holds_but_updates_pass() {
    let mut table = PresenceTable::new(2);
    table.track("t");
    assert!(table.insert("t", p("a", "1"), None));
    assert!(table.insert("t", p("a", "2"), None));
    assert!(!table.insert("t", p("a", "3"), None), "full");
    let mut updated = p("a", "2");
    updated.meta = json!({"status": "away"});
    assert!(table.insert("t", updated.clone(), None), "same member");
    assert_eq!(table.list("t")[1], updated);
    table.untrack("t");
    assert_eq!(table.count("t"), 0);
  }

  #[test]
  fn apply_reports_joins_leaves_and_changed_metadata() {
    let mut table = PresenceTable::new(8);
    table.track("t");
    let diff = table.apply("t", "b", vec![p("b", "1"), p("b", "2")], &[], LATER);
    assert_eq!(diff.joins.len(), 2);
    let again = table.apply("t", "b", vec![p("b", "1")], &[], LATER);
    assert!(again.is_empty(), "unchanged member is no diff");
    let mut moved = p("b", "1");
    moved.meta = json!(null);
    let changed = table.apply("t", "b", vec![moved.clone()], &["2".to_string()], LATER);
    assert_eq!(changed.joins, vec![moved]);
    assert_eq!(changed.leaves.len(), 2, "the old entry and conn 2");
  }

  #[test]
  fn replace_node_swaps_one_nodes_members_only() {
    let mut table = PresenceTable::new(8);
    table.track("t");
    table.insert("t", p("a", "1"), None);
    table.apply("t", "b", vec![p("b", "1"), p("b", "2")], &[], LATER);
    let diff = table.replace_node("t", "b", vec![p("b", "2"), p("b", "3")], LATER * 2);
    assert_eq!(diff.joins, vec![p("b", "3")]);
    assert_eq!(diff.leaves, vec![p("b", "1")]);
    assert_eq!(table.count("t"), 3);
    // b's entries were refreshed to the new expiry: nothing lapses at LATER.
    assert!(table.expire(LATER).is_empty());
    let lapsed = table.expire(LATER * 2);
    assert_eq!(lapsed.len(), 1);
    assert_eq!(lapsed[0].1.leaves.len(), 2);
    assert_eq!(
      table.list("t"),
      vec![p("a", "1")],
      "local members never lapse"
    );
  }

  #[test]
  fn drop_node_reports_per_topic() {
    let mut table = PresenceTable::new(8);
    table.track("t");
    table.track("u");
    table.apply("t", "b", vec![p("b", "1")], &[], LATER);
    table.apply("u", "b", vec![p("b", "2")], &[], LATER);
    table.insert("u", p("a", "1"), None);
    let gone = table.drop_node("b");
    assert_eq!(gone.len(), 2);
    assert_eq!(table.count("u"), 1);
    assert!(table.drop_node("b").is_empty());
  }

  #[test]
  fn wire_round_trip_keeps_metadata() {
    let wire = PresenceWire::of(&p("a", "1"));
    assert_eq!(wire.clone().into_presence("a"), p("a", "1"));
    let broken = PresenceWire {
      meta: "not json".into(),
      ..wire
    };
    assert_eq!(broken.into_presence("a").meta, Value::Null);
  }
}
