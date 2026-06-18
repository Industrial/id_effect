//! Reactive shared cell — Effect.ts `SubscriptionRef`: [`Ref`] plus change notifications via [`PubSub`].

use crate::coordination::{PubSub, Queue, Ref};
use crate::kernel::{Effect, box_future};
use crate::resource::Scope;
use crate::runtime::{Never, run_blocking};

/// Shared mutable cell that publishes every new value to subscribers.
#[derive(Clone)]
pub struct SubscriptionRef<A: Clone + Send + 'static> {
  ref_: Ref<A>,
  pubsub: PubSub<A>,
}

impl<A: Clone + Send + Sync + 'static> SubscriptionRef<A> {
  /// Allocate a cell with `value` and an unbounded change hub.
  pub fn make(value: A) -> Effect<SubscriptionRef<A>, Never, ()> {
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let ref_ = run_blocking(Ref::make(value), ()).expect("ref");
        let pubsub = run_blocking(PubSub::<A>::unbounded(), ()).expect("pubsub");
        Ok(SubscriptionRef { ref_, pubsub })
      })
    })
  }

  /// Read the current value.
  pub fn get(&self) -> Effect<A> {
    self.ref_.get()
  }

  /// Replace the value and notify subscribers.
  pub fn set(&self, value: A) -> Effect<()> {
    let ref_ = self.ref_.clone();
    let pubsub = self.pubsub.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        run_blocking(ref_.set(value.clone()), ()).expect("set");
        let _ = run_blocking(pubsub.publish(value), ());
        Ok(())
      })
    })
  }

  /// Apply `f` to the current value and notify subscribers with the new value.
  pub fn update(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<()> {
    let ref_ = self.ref_.clone();
    let pubsub = self.pubsub.clone();
    Effect::new_async(move |_r: &mut ()| {
      box_future(async move {
        let new_val = run_blocking(ref_.update_and_get(f), ()).expect("update");
        let _ = run_blocking(pubsub.publish(new_val), ());
        Ok(())
      })
    })
  }

  /// Subscribe to changes. The queue receives the current value first, then every subsequent update.
  pub fn subscribe(&self) -> Effect<Queue<A>, Never, Scope> {
    let ref_ = self.ref_.clone();
    let pubsub = self.pubsub.clone();
    Effect::new_async(move |scope: &mut Scope| {
      let scope = scope.clone();
      box_future(async move {
        let q = run_blocking(pubsub.subscribe(), scope).expect("subscribe");
        let current = run_blocking(ref_.get(), ()).expect("get");
        run_blocking(q.offer(current), ()).expect("offer current");
        Ok(q)
      })
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::runtime::run_async;

  #[tokio::test]
  async fn subscription_ref_notifies_subscribers() {
    let sr = run_async(SubscriptionRef::make(0u32), ())
      .await
      .expect("make");
    let scope = Scope::make();
    let q = run_async(sr.subscribe(), scope.clone())
      .await
      .expect("subscribe");

    assert_eq!(run_async(q.take(), ()).await.expect("current"), 0);
    run_async(sr.set(1), ()).await.expect("set");
    tokio::task::yield_now().await;
    assert_eq!(run_async(q.take(), ()).await.expect("change"), 1);
    scope.close();
  }

  #[tokio::test]
  async fn subscription_ref_get_reads_current() {
    let sr = run_async(SubscriptionRef::make(99u32), ())
      .await
      .expect("make");
    assert_eq!(run_async(sr.get(), ()).await.expect("get"), 99);
  }

  #[tokio::test]
  async fn subscription_ref_update_publishes() {
    let sr = run_async(SubscriptionRef::make(10u32), ())
      .await
      .expect("make");
    let scope = Scope::make();
    let q = run_async(sr.subscribe(), scope.clone())
      .await
      .expect("subscribe");
    let _ = run_async(q.take(), ()).await;
    run_async(sr.update(|n| n + 5), ()).await.expect("update");
    tokio::task::yield_now().await;
    assert_eq!(run_async(q.take(), ()).await.expect("change"), 15);
    scope.close();
  }
}
