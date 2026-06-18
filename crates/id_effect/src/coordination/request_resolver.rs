//! Batched request resolution — Effect.ts `RequestResolver` subset.
//!
//! A [`RequestResolver`] receives nested batches: outer `Vec` runs sequentially, inner entries
//! may run in parallel. Each [`RequestEntry`] carries a [`Deferred`] completed when the resolver
//! finishes the batch.

use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;

use crate::coordination::Deferred;
use crate::failure::cause::Cause;
use crate::kernel::{Effect, box_future};
use crate::runtime::{Never, run_blocking};

/// One pending lookup completed by [`RequestResolver::run_all`].
pub struct RequestEntry<K, V, E> {
  /// Lookup key.
  pub key: K,
  /// Result slot filled by the resolver.
  pub deferred: Deferred<V, E>,
}

/// Executes grouped requests: sequential batches, parallel entries within each batch.
pub trait RequestResolver<K, V, E, R = ()>
where
  K: Send + 'static,
  V: Clone + Send + 'static,
  E: Clone + Send + Sync + std::fmt::Debug + 'static,
  R: Send + 'static,
{
  /// Fulfill every entry in `batches`.
  fn run_all(&self, batches: Vec<Vec<RequestEntry<K, V, E>>>) -> Effect<(), Never, R>;
}

/// Closure-backed [`RequestResolver`].
pub struct FnRequestResolver<K, V, E, R, F> {
  run: F,
  _pd: PhantomData<fn(K, V, E, R)>,
}

impl<K, V, E, R, F> RequestResolver<K, V, E, R> for FnRequestResolver<K, V, E, R, F>
where
  K: Send + 'static,
  V: Clone + Send + 'static,
  E: Clone + Send + Sync + std::fmt::Debug + 'static,
  R: Send + 'static,
  F: Fn(Vec<Vec<RequestEntry<K, V, E>>>) -> Effect<(), Never, R> + Send + Sync,
{
  fn run_all(&self, batches: Vec<Vec<RequestEntry<K, V, E>>>) -> Effect<(), Never, R> {
    (self.run)(batches)
  }
}

/// Build a resolver from `run_all`.
pub fn make<K, V, E, R, F>(run_all: F) -> FnRequestResolver<K, V, E, R, F>
where
  K: Send + 'static,
  V: Clone + Send + 'static,
  E: Clone + Send + Sync + std::fmt::Debug + 'static,
  R: Send + 'static,
  F: Fn(Vec<Vec<RequestEntry<K, V, E>>>) -> Effect<(), Never, R> + Send + Sync,
{
  FnRequestResolver {
    run: run_all,
    _pd: PhantomData,
  }
}

/// Resolver error when a batch fetch omits a requested key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingKey<K>(pub K);

/// Deduplicate keys within each parallel batch and call `fetch` once per batch.
pub fn batching<K, V, E, F>(
  fetch: F,
) -> FnRequestResolver<
  K,
  V,
  MissingKey<K>,
  (),
  impl Fn(Vec<Vec<RequestEntry<K, V, MissingKey<K>>>>) -> Effect<(), Never, ()> + Send + Sync,
>
where
  K: Clone + Eq + Hash + Send + Sync + std::fmt::Debug + 'static,
  V: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + std::fmt::Debug + 'static,
  F: Fn(Vec<K>) -> Effect<HashMap<K, V>, E, ()> + Send + Sync + 'static,
{
  let fetch = std::sync::Arc::new(fetch);
  make::<K, V, MissingKey<K>, (), _>(move |batches| {
    let fetch = std::sync::Arc::clone(&fetch);
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        for batch in batches {
          let mut keys = Vec::new();
          for entry in &batch {
            if !keys.iter().any(|k| k == &entry.key) {
              keys.push(entry.key.clone());
            }
          }
          let map = run_blocking(fetch.as_ref()(keys), ()).expect("batch fetch");
          for entry in batch {
            match map.get(&entry.key) {
              Some(v) => {
                entry.deferred.try_succeed(v.clone());
              }
              None => {
                entry
                  .deferred
                  .try_fail_cause(Cause::fail(MissingKey(entry.key.clone())));
              }
            }
          }
        }
        Ok(())
      })
    })
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::run_async;
  use std::sync::{Arc, Mutex};

  #[tokio::test]
  async fn batching_resolver_fetches_deduped_keys() {
    let calls = Arc::new(Mutex::new(Vec::<Vec<u32>>::new()));
    let calls_c = Arc::clone(&calls);
    let resolver = batching(move |keys| {
      calls_c.lock().unwrap().push(keys.clone());
      let mut map = HashMap::new();
      for k in keys {
        map.insert(k, format!("v{k}"));
      }
      Effect::<HashMap<u32, String>, (), ()>::new(move |_r| Ok(map))
    });

    let mk = |key: u32| async move {
      let d = run_async(Deferred::<String, MissingKey<u32>>::make(), ())
        .await
        .unwrap();
      (
        RequestEntry {
          key,
          deferred: d.clone(),
        },
        d,
      )
    };

    let (e1, d1) = mk(1).await;
    let (e2, d2) = mk(1).await;
    let (e3, d3) = mk(2).await;

    run_async(resolver.run_all(vec![vec![e1, e2, e3]]), ())
      .await
      .expect("run_all");

    assert_eq!(calls.lock().unwrap().as_slice(), &[vec![1, 2]]);
    assert_eq!(
      run_async(d1.wait(), ()).await.expect("d1"),
      "v1".to_string()
    );
    assert_eq!(
      run_async(d2.wait(), ()).await.expect("d2"),
      "v1".to_string()
    );
    assert_eq!(
      run_async(d3.wait(), ()).await.expect("d3"),
      "v2".to_string()
    );
  }
}
