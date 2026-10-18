//! Stages two orders in a `MemoryOutbox`, relays them to a `MemoryBroker`, publishes one again
//! as a crashed relay would, and consumes the topic through a `MemoryInbox`, which processes each
//! message once. On PostgreSQL the same flow is `Outbox::stage` in the caller's transaction,
//! `PgOutbox` for the relay and `Inbox::handler`.
//!
//! `cargo run -p id_effect_messaging --example 030_outbox_inbox`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::succeed;
use id_effect_messaging::{
  ConsumerGroup, MemoryBroker, MemoryInbox, MemoryOutbox, Message, MessageBroker, Relay,
  SubscribeOptions, Topic,
};

fn main() {
  let orders = Topic::new("orders").unwrap();
  let outbox = MemoryOutbox::new();
  for id in ["o-1", "o-2"] {
    let staged = outbox
      .stage(&orders, Message::new(id).with_id(id))
      .block_on();
    assert!(staged.into_result().is_ok());
  }

  let broker = MemoryBroker::new();
  let relay = Relay::new(Arc::new(outbox.clone()), Arc::new(broker.clone()));
  let report = relay.run_until_idle().block_on().into_result().unwrap();
  println!("relayed {} of {} claimed", report.published, report.claimed);
  assert_eq!(outbox.sent(), 2);
  // At least once: a relay that crashed after publishing o-1 publishes it again.
  let again = broker.publish(&orders, Message::new("o-1").with_id("o-1"));
  assert!(again.block_on().into_result().is_ok());

  let inbox = MemoryInbox::new();
  let processed = Arc::new(AtomicUsize::new(0));
  let handler = inbox.handler({
    let processed = Arc::clone(&processed);
    move |m: &Message| {
      println!("processing {}", m.id());
      processed.fetch_add(1, Ordering::SeqCst);
      succeed::<(), (), _>(())
    }
  });
  let billing = ConsumerGroup::new("billing").unwrap();
  let consumed = broker
    .subscribe(&orders, &billing, SubscribeOptions::default())
    .flat_map(move |deliveries| deliveries.take(3).run_for_each(handler))
    .block_on();
  assert!(consumed.into_result().is_ok());
  println!(
    "3 deliveries, {} processed",
    processed.load(Ordering::SeqCst)
  );
  assert_eq!(processed.load(Ordering::SeqCst), 2);
}
