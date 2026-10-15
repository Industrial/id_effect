//! [`SubscriptionRef`]: a [`Ref`] that publishes every new value to its subscribers (Effect.ts
//! `SubscriptionRef`). Ported from the v1 `coordination::subscription_ref`, which drove its
//! inner effects with `run_blocking`.
//!
//! Writes and subscriptions are serialized by a one-permit [`Semaphore`], so a subscriber sees
//! the value current at subscription first, then every later value in write order, with no gap
//! and no duplicate.

use super::pubsub::PubSub;
use super::queue::Queue;
use super::ref_::Ref;
use super::semaphore::Semaphore;
use crate::kernel::Effect;
use crate::scope::Scope;

/// A shared cell whose changes can be subscribed to. Cloning yields another handle to the same
/// cell.
pub struct SubscriptionRef<A> {
  cell: Ref<A>,
  hub: PubSub<A>,
  gate: Semaphore,
}

impl<A> Clone for SubscriptionRef<A> {
  fn clone(&self) -> Self {
    Self {
      cell: self.cell.clone(),
      hub: self.hub.clone(),
      gate: self.gate.clone(),
    }
  }
}

impl<A: std::fmt::Debug> std::fmt::Debug for SubscriptionRef<A> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SubscriptionRef")
      .field("value", &self.cell)
      .field("subscribers", &self.hub.subscriber_count())
      .finish()
  }
}

impl<A: Clone + Send + 'static> SubscriptionRef<A> {
  /// A new cell holding `value`, with an unbounded change hub.
  pub fn new(value: A) -> Self {
    Self {
      cell: Ref::new(value),
      hub: PubSub::unbounded(),
      gate: Semaphore::new(1),
    }
  }

  /// Reads the current value.
  pub fn get<E, R>(&self) -> Effect<A, E, R> {
    self.cell.get()
  }

  /// Replaces the value and notifies the subscribers.
  pub fn set<E, R>(&self, value: A) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let hub = self.hub.clone();
    let v = value.clone();
    self
      .gate
      .with_permit(self.cell.set(value).then(hub.publish(v)).map(drop))
  }

  /// Replaces the value with `f(current)` and notifies the subscribers with the new value.
  pub fn update<E, R>(&self, f: impl FnOnce(A) -> A + Send + 'static) -> Effect<(), E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let hub = self.hub.clone();
    self.gate.with_permit(
      self
        .cell
        .update_and_get(f)
        .flat_map(move |new| hub.publish(new))
        .map(drop),
    )
  }

  /// Computes `(b, new)` from the current value, stores `new`, notifies the subscribers and
  /// returns `b`.
  pub fn modify<B, E, R>(&self, f: impl FnOnce(A) -> (B, A) + Send + 'static) -> Effect<B, E, R>
  where
    B: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let hub = self.hub.clone();
    self.gate.with_permit(
      self
        .cell
        .modify(move |a| {
          let (b, new) = f(a);
          ((b, new.clone()), new)
        })
        .flat_map(move |(b, new)| hub.publish(new).map(move |_| b)),
    )
  }

  /// Subscribes to changes: the queue receives the current value first, then every later value.
  /// The subscription ends when `scope` closes.
  pub fn subscribe<E, R>(&self, scope: &Scope) -> Effect<Queue<A>, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    let cell = self.cell.clone();
    self
      .gate
      .with_permit(self.hub.subscribe(scope).flat_map(move |q: Queue<A>| {
        let q2 = q.clone();
        cell
          .get()
          .flat_map(move |current| q2.offer(current))
          .map(move |_| q)
      }))
  }
}
