//! [`Delivery`]: one message handed to one subscription, which settles it once with
//! [`ack`](Delivery::ack) or [`nack`](Delivery::nack) through the broker's [`Acker`].

use std::fmt;
use std::sync::Arc;

use id_effect::schema::{HasSchema, Unknown};
use id_effect::{Effect, succeed, suspend};

use crate::Topic;
use crate::broker::{BrokerError, ConsumerGroup, Message};

/// What to do with a delivery the consumer did not process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nack {
  /// Deliver it again (to any subscription of the group), unless this was its
  /// [`max_deliveries`](crate::SubscribeOptions::max_deliveries)-th delivery: then dead-letter it
  /// ([`DeadLetterReason::MaxDeliveries`]).
  Requeue,
  /// Retrying cannot help (a poison message): dead-letter it now
  /// ([`DeadLetterReason::Rejected`]).
  DeadLetter,
}

/// Why a message was moved to a dead-letter topic (its
/// [`DEAD_LETTER_REASON`](crate::headers::DEAD_LETTER_REASON) header).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeadLetterReason {
  /// The consumer nacked it with [`Nack::DeadLetter`].
  Rejected,
  /// It was delivered [`max_deliveries`](crate::SubscribeOptions::max_deliveries) times without
  /// an ack.
  MaxDeliveries,
}

impl DeadLetterReason {
  /// The header value: `rejected` or `max-deliveries`.
  pub const fn as_str(self) -> &'static str {
    match self {
      DeadLetterReason::Rejected => "rejected",
      DeadLetterReason::MaxDeliveries => "max-deliveries",
    }
  }

  /// The reason of a header value.
  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "rejected" => Some(DeadLetterReason::Rejected),
      "max-deliveries" => Some(DeadLetterReason::MaxDeliveries),
      _ => None,
    }
  }
}

impl fmt::Display for DeadLetterReason {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(self.as_str())
  }
}

/// How a broker settles one delivery; a [`Delivery`] holds one and spends it on
/// [`ack`](Delivery::ack) or [`nack`](Delivery::nack). A broker implements it per delivery
/// (Redis Streams: `XACK` on ack, re-pending or `XADD` to the dead-letter stream on nack).
///
/// A delivery dropped unsettled (also when its settle effect is dropped before it runs) drops its
/// acker: an implementation may treat that as a [`Nack::Requeue`] in its `Drop`, as
/// [`MemoryBroker`](crate::MemoryBroker) does, or leave the message pending for the broker to
/// redeliver after a timeout.
pub trait Acker: Send + 'static {
  /// The consumer processed the message: the group is done with it.
  fn ack(self: Box<Self>) -> Effect<(), BrokerError>;

  /// The consumer did not process the message.
  fn nack(self: Box<Self>, nack: Nack) -> Effect<(), BrokerError>;
}

/// One message delivered to a subscription. Settle it once, with [`ack`](Self::ack) or
/// [`nack`](Self::nack), and run the returned effect; until then it counts against the
/// subscription's [`prefetch`](crate::SubscribeOptions::prefetch).
pub struct Delivery {
  message: Message,
  topic: Topic,
  group: ConsumerGroup,
  attempt: u32,
  acker: Box<dyn Acker>,
}

impl fmt::Debug for Delivery {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Delivery")
      .field("id", self.message.id())
      .field("topic", &self.topic)
      .field("group", &self.group)
      .field("attempt", &self.attempt)
      .finish_non_exhaustive()
  }
}

impl Delivery {
  /// A delivery of `message` from `topic` to `group`, its `attempt`-th (from 1), settled through
  /// `acker` (what a broker builds).
  pub fn new(
    message: Message,
    topic: Topic,
    group: ConsumerGroup,
    attempt: u32,
    acker: Box<dyn Acker>,
  ) -> Self {
    Self {
      message,
      topic,
      group,
      attempt,
      acker,
    }
  }

  /// The message.
  pub fn message(&self) -> &Message {
    &self.message
  }

  /// The topic it was published to.
  pub fn topic(&self) -> &Topic {
    &self.topic
  }

  /// The group it was delivered to.
  pub fn group(&self) -> &ConsumerGroup {
    &self.group
  }

  /// How many times the group has delivered the message, this delivery included (from 1).
  pub fn attempt(&self) -> u32 {
    self.attempt
  }

  /// Whether the group delivered the message before ([`attempt`](Self::attempt) above 1).
  pub fn is_redelivery(&self) -> bool {
    self.attempt > 1
  }

  /// The payload decoded with `T`'s schema ([`Message::decode`]).
  pub fn decode<T>(&self) -> Result<T, BrokerError>
  where
    T: HasSchema<A = T, I = Unknown, E = ()>,
  {
    self.message.decode()
  }

  /// Settles it as processed.
  pub fn ack(self) -> Effect<(), BrokerError> {
    self.acker.ack()
  }

  /// Settles it as not processed: redeliver or dead-letter (see [`Nack`]).
  pub fn nack(self, nack: Nack) -> Effect<(), BrokerError> {
    self.acker.nack(nack)
  }
}

/// A [`consume`](crate::consume) handler that runs `handler` over each delivery and settles it:
/// [`ack`](Delivery::ack) when the handler succeeds, [`Nack::Requeue`] when it fails (so a
/// message that keeps failing is dead-lettered after
/// [`max_deliveries`](crate::SubscribeOptions::max_deliveries)). The handler's error is dropped;
/// a handler that dies or is interrupted leaves the delivery to be redelivered.
pub fn ack_on_success<E, F>(
  handler: F,
) -> impl Fn(Delivery) -> Effect<(), BrokerError> + Send + Sync + 'static
where
  E: Send + 'static,
  F: Fn(&Delivery) -> Effect<(), E> + Send + Sync + 'static,
{
  let handler = Arc::new(handler);
  move |delivery: Delivery| {
    let handler = Arc::clone(&handler);
    // `consume` builds a chunk's effects before running them: call the handler when this runs.
    suspend(move || {
      handler(&delivery)
        .map(|()| true)
        .catch(|_| succeed(false))
        .flat_map(move |processed| {
          if processed {
            delivery.ack()
          } else {
            delivery.nack(Nack::Requeue)
          }
        })
    })
  }
}

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Mutex};

  use id_effect::{Exit, fail};

  use super::*;

  /// Records how each delivery was settled.
  struct Recorder(Arc<Mutex<Vec<String>>>);

  impl Acker for Recorder {
    fn ack(self: Box<Self>) -> Effect<(), BrokerError> {
      self.0.lock().unwrap().push("ack".into());
      succeed(())
    }

    fn nack(self: Box<Self>, nack: Nack) -> Effect<(), BrokerError> {
      self.0.lock().unwrap().push(format!("{nack:?}"));
      succeed(())
    }
  }

  fn delivery(log: &Arc<Mutex<Vec<String>>>, attempt: u32) -> Delivery {
    Delivery::new(
      Message::new("p").with_id("m-1"),
      Topic::new("orders").unwrap(),
      ConsumerGroup::new("billing").unwrap(),
      attempt,
      Box::new(Recorder(Arc::clone(log))),
    )
  }

  #[test]
  fn accessors_and_settling() {
    let log = Arc::default();
    let d = delivery(&log, 1);
    assert_eq!(d.message().id().as_str(), "m-1");
    assert_eq!(
      (d.topic().as_str(), d.group().as_str()),
      ("orders", "billing")
    );
    assert_eq!((d.attempt(), d.is_redelivery()), (1, false));
    assert!(format!("{d:?}").contains("attempt: 1"));
    assert!(matches!(d.ack().block_on(), Exit::Success(())));
    let d = delivery(&log, 2);
    assert!(d.is_redelivery());
    assert!(matches!(
      d.nack(Nack::DeadLetter).block_on(),
      Exit::Success(())
    ));
    assert_eq!(*log.lock().unwrap(), ["ack", "DeadLetter"]);
  }

  #[test]
  fn ack_on_success_acks_or_requeues() {
    let log = Arc::default();
    let handler = ack_on_success(|d: &Delivery| {
      if d.attempt() == 1 {
        fail("boom")
      } else {
        succeed(())
      }
    });
    assert!(matches!(
      handler(delivery(&log, 1)).block_on(),
      Exit::Success(())
    ));
    assert!(matches!(
      handler(delivery(&log, 2)).block_on(),
      Exit::Success(())
    ));
    assert_eq!(*log.lock().unwrap(), ["Requeue", "ack"]);
  }

  #[test]
  fn reasons_round_trip() {
    for r in [DeadLetterReason::Rejected, DeadLetterReason::MaxDeliveries] {
      assert_eq!(DeadLetterReason::parse(r.as_str()), Some(r));
      assert_eq!(r.to_string(), r.as_str());
    }
    assert_eq!(DeadLetterReason::parse("other"), None);
  }
}
