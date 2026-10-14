//! Integration tests for Fabric-aware implicit parallelism in fp collections (primary + serial APIs).

use id_effect::compute::{ComputeFabric, install_fabric};
use id_effect_fp::{RedBlackTree, Trie, algebra::functor::vec, hash_map, hash_set};
use std::sync::Arc;

fn install_test_fabric() {
  install_fabric(Arc::new(ComputeFabric::memory_cap_max_cpu(1.0)));
}

#[test]
fn parallel_dispatch_integration() {
  install_test_fabric();

  let input: Vec<i32> = (0..128).collect();
  let out = vec::map(input.clone(), |x| x + 1);
  assert_eq!(out.len(), 128);
  assert_eq!(out[0], 1);
  assert_eq!(vec::map_serial(input, |x| x + 1), out);

  let m = hash_map::from_iter([(1_i32, 10), (2, 20), (3, 30)]);
  let mapped = hash_map::map_values(m.clone(), |v| v * 2);
  assert_eq!(hash_map::get(&mapped, &2), Some(&40));
  assert_eq!(hash_map::map_values_serial(m.clone(), |v| v * 2), mapped);
  let filtered = hash_map::filter(&m, |k, _| *k > 1);
  assert!(!hash_map::has(&filtered, &1));
  assert_eq!(hash_map::filter_serial(&m, |k, _| *k > 1), filtered);

  let mut t = RedBlackTree::empty();
  for i in 0..64_i32 {
    t.insert(i, i * 10);
  }
  assert_eq!(t.entries(), t.entries_serial());
  assert_eq!(t.size(), t.size_serial());
  assert_eq!(t.greater_than(&32), t.greater_than_serial(&32));
  assert_eq!(t.less_than(&32), t.less_than_serial(&32));
  assert_eq!(t.values(), t.values_serial());

  let large: Vec<i32> = (0..2048).collect();
  let large_out = vec::map(large.clone(), |x| x + 1);
  assert_eq!(large_out.len(), 2048);
  assert_eq!(large_out[2047], 2048);

  let mut small = RedBlackTree::empty();
  for i in 0..32_i32 {
    small.insert(i, i * 2);
  }
  let _ = small.greater_than_serial(&16);
  let _ = small.greater_than(&16);
  let _ = small.less_than_serial(&16);
  let _ = small.less_than(&16);
  let _ = small.size();
  let _ = small.entries();
  let _ = small.values();
}

#[test]
fn large_collection_parallel_paths() {
  install_test_fabric();

  let large: Vec<i32> = (0..3000).collect();
  let mapped = vec::map(large.clone(), |x| x + 1);
  assert_eq!(mapped, vec::map_serial(large, |x| x + 1));

  let pairs: Vec<(i32, i32)> = (0..3000).map(|i| (i, i * 2)).collect();
  let m = hash_map::from_iter(pairs);
  assert_eq!(
    hash_map::map_values(m.clone(), |v| v + 1),
    hash_map::map_values_serial(m.clone(), |v| v + 1)
  );
  assert_eq!(
    hash_map::filter(&m, |k, _| *k % 2 == 0),
    hash_map::filter_serial(&m, |k, _| *k % 2 == 0)
  );

  let set = hash_set::from_iter(0..3000_i32);
  assert_eq!(hash_set::values(&set), hash_set::values_serial(&set));

  let mut trie = Trie::empty();
  let keys: Vec<String> = (0..3000_i32).map(|i| format!("key-{i}")).collect();
  for (i, key) in keys.iter().enumerate() {
    trie.insert(key, i as i32);
  }
  assert_eq!(trie.size(), trie.size_serial());

  let mut tree = RedBlackTree::empty();
  for i in 0..3000_i32 {
    tree.insert(i, i * 2);
  }
  assert_eq!(tree.size(), tree.size_serial());
  assert_eq!(tree.entries(), tree.entries_serial());
  assert_eq!(tree.values(), tree.values_serial());
  assert_eq!(tree.greater_than(&0), tree.greater_than_serial(&0));
  assert_eq!(tree.less_than(&3000), tree.less_than_serial(&3000));
}
