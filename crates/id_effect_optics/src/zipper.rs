//! [`TrieZipper`] — navigable zipper over an immutable trie (stub for Part V ch18).

use im::HashMap;

/// Persistent trie node keyed by single-character segments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrieNode<V> {
  /// Optional value stored at this node.
  pub value: Option<V>,
  /// Outgoing edges keyed by next character.
  pub children: HashMap<char, TrieNode<V>>,
}

impl<V> TrieNode<V> {
  /// Empty trie node.
  pub fn new() -> Self {
    Self {
      value: None,
      children: HashMap::new(),
    }
  }

  /// Node with a value and no children.
  pub fn leaf(value: V) -> Self {
    Self {
      value: Some(value),
      children: HashMap::new(),
    }
  }

  /// Follow a child edge when present.
  pub fn child(&self, key: char) -> Option<&TrieNode<V>> {
    self.children.get(&key)
  }
}

/// Zipper focus into a [`TrieNode`] with a breadcrumb path (stub API).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrieZipper<V> {
  focus: TrieNode<V>,
  path: Vec<(char, TrieNode<V>)>,
}

impl<V: Clone> TrieZipper<V> {
  /// Start at the root node.
  pub fn new(root: TrieNode<V>) -> Self {
    Self {
      focus: root,
      path: Vec::new(),
    }
  }

  /// Borrow the focused node.
  pub fn focus(&self) -> &TrieNode<V> {
    &self.focus
  }

  /// Move focus down a child edge when it exists.
  pub fn descend(&self, key: char) -> Option<Self> {
    let child = self.focus.child(key)?.clone();
    let mut path = self.path.clone();
    path.push((key, self.focus.clone()));
    Some(Self { focus: child, path })
  }

  /// Move focus to the parent when not at root.
  pub fn ascend(&self) -> Option<Self> {
    let (_key, parent) = self.path.last()?.clone();
    let path = self.path[..self.path.len() - 1].to_vec();
    Some(Self {
      focus: parent,
      path,
    })
  }

  /// Rebuild the trie from the zipper focus (stub: returns focus subtree only).
  pub fn rebuild(&self) -> TrieNode<V> {
    self.focus.clone()
  }

  /// Insert or replace a value at the focused node.
  pub fn set_value(&self, value: V) -> Self {
    let mut focus = self.focus.clone();
    focus.value = Some(value);
    Self {
      focus,
      path: self.path.clone(),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn sample_trie() -> TrieNode<&'static str> {
    let mut root = TrieNode::new();
    let mut a = TrieNode::leaf("root-a");
    let b = TrieNode::leaf("root-a-b");
    a.children.insert('b', b);
    root.children.insert('a', a);
    root
  }

  mod descend {
    use super::*;

    #[test]
    fn moves_focus_to_child() {
      let zipper = TrieZipper::new(sample_trie());
      let child = zipper.descend('a').expect("child");
      assert_eq!(child.focus().value, Some("root-a"));
    }

    #[test]
    fn returns_none_for_missing_edge() {
      let zipper = TrieZipper::new(sample_trie());
      assert!(zipper.descend('z').is_none());
    }
  }

  mod ascend {
    use super::*;

    #[test]
    fn returns_to_parent() {
      let zipper = TrieZipper::new(sample_trie());
      let child = zipper.descend('a').unwrap();
      let parent = child.ascend().unwrap();
      assert!(parent.focus().child('a').is_some());
    }

    #[test]
    fn returns_none_at_root() {
      let zipper = TrieZipper::new(sample_trie());
      assert!(zipper.ascend().is_none());
    }
  }

  mod rebuild {
    use super::*;

    #[test]
    fn rebuild_round_trip() {
      let root = TrieNode::leaf("v");
      let zipper = TrieZipper::new(root).set_value("new");
      let rebuilt = zipper.rebuild();
      assert_eq!(rebuilt.value, Some("new"));
    }

    #[test]
    fn trie_node_child_lookup() {
      let mut node = TrieNode::new();
      node.children.insert('x', TrieNode::leaf(1));
      assert_eq!(node.child('x').unwrap().value, Some(1));
      assert!(node.child('y').is_none());
    }
  }

  mod set_value {
    use super::*;

    #[test]
    fn updates_focused_node() {
      let zipper = TrieZipper::new(sample_trie()).set_value("root");
      assert_eq!(zipper.focus().value, Some("root"));
    }
  }
}
