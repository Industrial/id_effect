//! Trie zipper navigation and rebuild.

use id_effect_optics::{TrieNode, TrieZipper};

fn main() {
  let root = TrieNode::new().with_child('a', TrieNode::leaf("leaf"));
  let rebuilt = TrieZipper::new(root)
    .descend('a')
    .unwrap()
    .set_value("updated")
    .rebuild();
  println!("value = {:?}", rebuilt.child('a').unwrap().value);
}
