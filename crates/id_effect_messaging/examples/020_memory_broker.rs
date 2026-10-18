//! Publishes three orders to a `MemoryBroker`, consumes them in a group with prefetch 2 and two
//! handlers at once, and dead-letters the one that keeps failing.
//!
//! `cargo run -p id_effect_messaging --example 020_memory_broker`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::{Effect, fail, succeed};
use id_effect_messaging::{
  ConsumerGroup, Delivery, MemoryBroker, Message, MessageBroker, MessageBrokerExt,
  SubscribeOptions, Topic, ack_on_success, headers,
};

fn main() {
  let broker = MemoryBroker::new();
  let orders = Topic::new("orders").unwrap();
  let billing = ConsumerGroup::new("billing").unwrap();
  for (id, body) in [("o-1", "ok"), ("o-2", "poison"), ("o-3", "ok")] {
    let published = broker
      .publish(&orders, Message::new(body).with_id(id))
      .block_on();
    assert!(published.into_result().is_ok());
  }

  // o-1 and o-3 once each, o-2 three times (max_deliveries): five deliveries, then stop.
  let seen = Arc::new(AtomicUsize::new(0));
  let handler = {
    let (broker, seen) = (broker.clone(), Arc::clone(&seen));
    ack_on_success(move |d: &Delivery| -> Effect<(), String> {
      println!("{} attempt {}", d.message().id(), d.attempt());
      if seen.fetch_add(1, Ordering::SeqCst) + 1 == 5 {
        broker.close();
      }
      if d.message().payload() == b"poison" {
        fail("cannot bill".into())
      } else {
        succeed(())
      }
    })
  };
  let opts = SubscribeOptions::default()
    .prefetch(2)
    .concurrency(2)
    .max_deliveries(3);
  let done = broker.consume(&orders, &billing, opts, handler).block_on();
  assert!(done.into_result().is_ok());

  for dead in broker.messages(&orders.dead_letter().unwrap()) {
    println!(
      "dead-lettered {} after {} deliveries ({})",
      dead.id(),
      dead.header(headers::DELIVERY_COUNT).unwrap_or("?"),
      dead.header(headers::DEAD_LETTER_REASON).unwrap_or("?"),
    );
  }
}
