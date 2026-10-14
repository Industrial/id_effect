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
use crate::runtime::Never;

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

/// Per-entry failure produced by a [`batching`] resolver.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BatchError<K, E> {
  /// The batch fetch succeeded but its map omitted this key.
  Missing(K),
  /// The batch fetch failed; every entry still waiting receives the fetch error.
  Fetch(E),
}

/// Deduplicate keys within each parallel batch and await `fetch` once per batch.
///
/// If a fetch fails with `e`, every entry of that batch and of all later batches fails with
/// [`BatchError::Fetch`]`(e)` and no further fetch runs.
pub fn batching<K, V, E, F>(
  fetch: F,
) -> FnRequestResolver<
  K,
  V,
  BatchError<K, E>,
  (),
  impl Fn(Vec<Vec<RequestEntry<K, V, BatchError<K, E>>>>) -> Effect<(), Never, ()> + Send + Sync,
>
where
  K: Clone + Eq + Hash + Send + Sync + std::fmt::Debug + 'static,
  V: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + std::fmt::Debug + 'static,
  F: Fn(Vec<K>) -> Effect<HashMap<K, V>, E, ()> + Send + Sync + 'static,
{
  let fetch = std::sync::Arc::new(fetch);
  make::<K, V, BatchError<K, E>, (), _>(move |batches| {
    let fetch = std::sync::Arc::clone(&fetch);
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let mut failed: Option<E> = None;
        for batch in batches {
          if let Some(e) = &failed {
            fail_batch(batch, e);
            continue;
          }
          let mut keys = Vec::new();
          for entry in &batch {
            if !keys.iter().any(|k| k == &entry.key) {
              keys.push(entry.key.clone());
            }
          }
          let map = match fetch.as_ref()(keys).run(&mut ()).await {
            Ok(map) => map,
            Err(e) => {
              fail_batch(batch, &e);
              failed = Some(e);
              continue;
            }
          };
          for entry in batch {
            match map.get(&entry.key) {
              Some(v) => {
                entry.deferred.try_succeed(v.clone());
              }
              None => {
                entry
                  .deferred
                  .try_fail_cause(Cause::fail(BatchError::Missing(entry.key.clone())));
              }
            }
          }
        }
        Ok(())
      })
    })
  })
}

fn fail_batch<K, V, E>(batch: Vec<RequestEntry<K, V, BatchError<K, E>>>, e: &E)
where
  K: Clone + Send + Sync + 'static,
  V: Clone + Send + Sync + 'static,
  E: Clone + Send + Sync + 'static,
{
  for entry in batch {
    entry
      .deferred
      .try_fail_cause(Cause::fail(BatchError::Fetch(e.clone())));
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::run_async;
  use std::sync::{Arc, Mutex};

  #[tokio::test]
  async fn batching_empty_keys() {
    let resolver = batching(|keys: Vec<u32>| {
      Effect::<HashMap<u32, String>, (), ()>::new(move |_| {
        let map: std::collections::HashMap<u32, String> =
          keys.into_iter().map(|k| (k, format!("v{k}"))).collect();
        Ok(map)
      })
    });
    run_async(resolver.run_all(vec![vec![]]), ())
      .await
      .expect("empty inner batch");
  }

  #[tokio::test]
  async fn make_resolver_empty_batches() {
    let resolver =
      make(|_batches: Vec<Vec<RequestEntry<u32, String, ()>>>| Effect::new(|_| Ok(())));
    run_async(resolver.run_all(vec![]), ())
      .await
      .expect("run_all");
  }

  async fn entry<E>(key: u32) -> (RequestEntry<u32, String, E>, Deferred<String, E>)
  where
    E: Clone + Send + Sync + 'static,
  {
    let d = run_async(Deferred::<String, E>::make(), ()).await.unwrap();
    (
      RequestEntry {
        key,
        deferred: d.clone(),
      },
      d,
    )
  }

  #[tokio::test(flavor = "current_thread")]
  async fn request_resolver_awaits_async_fetch_on_current_thread() {
    let resolver = batching(|keys: Vec<u32>| {
      Effect::<HashMap<u32, String>, (), ()>::new_async(move |_r| {
        box_future(async move {
          tokio::time::sleep(std::time::Duration::from_millis(1)).await;
          Ok(keys.into_iter().map(|k| (k, format!("v{k}"))).collect())
        })
      })
    });
    let (e1, d1) = entry(1).await;
    let (e2, d2) = entry(2).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
      run_async(resolver.run_all(vec![vec![e1], vec![e2]]), ())
        .await
        .expect("run_all");
    })
    .await
    .expect("resolver completes within 1s");
    assert_eq!(run_async(d1.wait(), ()).await.expect("d1"), "v1");
    assert_eq!(run_async(d2.wait(), ()).await.expect("d2"), "v2");
  }

  #[tokio::test]
  async fn request_resolver_fetch_error_fails_every_entry() {
    let calls = Arc::new(Mutex::new(0usize));
    let calls_c = Arc::clone(&calls);
    let resolver = batching(move |_keys: Vec<u32>| {
      *calls_c.lock().unwrap() += 1;
      Effect::<HashMap<u32, String>, &'static str, ()>::new(|_r| Err("db down"))
    });
    let (e1, d1) = entry(1).await;
    let (e2, d2) = entry(2).await;
    let (e3, d3) = entry(3).await;
    run_async(resolver.run_all(vec![vec![e1, e2], vec![e3]]), ())
      .await
      .expect("run_all");
    assert_eq!(*calls.lock().unwrap(), 1);
    for d in [d1, d2, d3] {
      assert_eq!(
        run_async(d.wait(), ()).await.unwrap_err(),
        Cause::fail(BatchError::Fetch("db down"))
      );
    }
  }

  #[tokio::test]
  async fn request_resolver_missing_key_fails_entry() {
    let resolver = batching(|_keys: Vec<u32>| {
      Effect::<HashMap<u32, String>, (), ()>::new(|_r| Ok(HashMap::from([(1, "v1".to_string())])))
    });
    let (e1, d1) = entry(1).await;
    let (e2, d2) = entry(2).await;
    run_async(resolver.run_all(vec![vec![e1, e2]]), ())
      .await
      .expect("run_all");
    assert_eq!(run_async(d1.wait(), ()).await.expect("d1"), "v1");
    assert_eq!(
      run_async(d2.wait(), ()).await.unwrap_err(),
      Cause::fail(BatchError::Missing(2))
    );
  }

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

    let (e1, d1) = entry(1).await;
    let (e2, d2) = entry(1).await;
    let (e3, d3) = entry(2).await;

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
