//! Ex 030 — id_effect_messaging's `MessageBroker` on Redis Streams (feature `streams`).
//!
//! Publishes three orders, consumes them in the group `billing` (one fails once and is
//! redelivered, one is a poison message nacked to the dead-letter stream), then reads the
//! dead-letter stream `example.orders.dlq`.
//!
//! Needs a Redis server at `REDIS_URL` (default `redis://127.0.0.1:6379`), e.g.
//! `docker run --rm -p 6379:6379 redis:8.2`. Without one it says so and exits successfully.
//!
//! Run: `cargo run -p id_effect_redis --features streams --example 030_streams`

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use id_effect::{Effect, Exit, succeed};
use id_effect_messaging::{
  BrokerError, ConsumerGroup, Delivery, Message, MessageBroker, MessageBrokerExt, Nack,
  SubscribeOptions, Topic, headers,
};
use id_effect_redis::{RedisClient, RedisConfig, RedisStreamsBroker, RedisStreamsConfig};
use id_effect_tokio::TokioRuntime;

fn program(redis: RedisClient) -> Effect<(), BrokerError> {
  let orders = Topic::new("example.orders").unwrap();
  let dlq = orders.dead_letter().unwrap();
  let billing = ConsumerGroup::new("billing").unwrap();
  let broker = RedisStreamsBroker::new(
    redis.clone(),
    RedisStreamsConfig::new()
      .consumer("example-1")
      .block(Duration::from_millis(200))
      .max_len(10_000),
  );
  // Start from empty streams on every run.
  let reset = redis
    .query::<i64>(
      redis::cmd("DEL")
        .arg(orders.as_str())
        .arg(dlq.as_str())
        .clone(),
    )
    .map(|_| ())
    .map_err(BrokerError::from);

  let publish = {
    let (broker, orders) = (broker.clone(), orders.clone());
    move |()| {
      let publish = |id: &str, body: &str| {
        broker.publish(&orders, Message::new(body).with_id(id).with_key("acct-1"))
      };
      publish("o-1", "order 1")
        .flat_map({
          let p = publish("o-2", "flaky order");
          move |()| p
        })
        .flat_map({
          let p = publish("o-3", "poison");
          move |()| p
        })
    }
  };

  let handled = Arc::new(AtomicUsize::new(0));
  let handler = {
    let (broker, handled) = (broker.clone(), Arc::clone(&handled));
    move |d: Delivery| {
      let text = d.message().text().unwrap_or_default().to_owned();
      println!("{} attempt {}: {text}", d.message().id(), d.attempt());
      // o-1, o-2 (failing), o-3 (poison), o-2 again: four deliveries.
      if handled.fetch_add(1, Ordering::SeqCst) + 1 == 4 {
        broker.close();
      }
      match text.as_str() {
        "poison" => d.nack(Nack::DeadLetter),
        "flaky order" if d.attempt() == 1 => d.nack(Nack::Requeue),
        _ => d.ack(),
      }
    }
  };
  let consume = {
    let (broker, orders, billing) = (broker.clone(), orders.clone(), billing.clone());
    move |()| {
      broker.consume(
        &orders,
        &billing,
        SubscribeOptions::default().prefetch(8),
        handler,
      )
    }
  };

  // The dead-letter stream is a topic like any other: read it with its own group.
  let read_dlq = move |()| {
    let broker = RedisStreamsBroker::new(redis, RedisStreamsConfig::new().consumer("example-1"));
    let audit = ConsumerGroup::new("audit").unwrap();
    broker
      .subscribe(&dlq, &audit, SubscribeOptions::default())
      .flat_map(|deliveries| {
        deliveries
          .take(1)
          .map_effect(|d: Delivery| {
            let m = d.message();
            println!(
              "dead-lettered {} from {} ({})",
              m.id(),
              m.header(headers::DEAD_LETTER_TOPIC).unwrap_or("?"),
              m.header(headers::DEAD_LETTER_REASON).unwrap_or("?"),
            );
            d.ack()
          })
          .run_collect()
      })
      .flat_map(|_| succeed(()))
  };

  reset.flat_map(publish).flat_map(consume).flat_map(read_dlq)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
  let url = std::env::var("REDIS_URL").unwrap_or_else(|_| RedisConfig::DEFAULT_URL.to_owned());
  let config = RedisConfig::new(url.clone())
    .connection_timeout(Duration::from_millis(500))
    .reconnect_retries(0);
  let runtime = TokioRuntime::current().expect("tokio runtime");
  let redis = match runtime.run(RedisClient::connect(config)).await {
    Exit::Success(redis) => redis,
    Exit::Failure(cause) => {
      println!("skipped: no Redis at {url} ({cause:?})");
      return;
    }
  };
  match runtime.run(program(redis)).await {
    Exit::Success(()) => {}
    Exit::Failure(cause) => panic!("example failed: {cause:?}"),
  }
}
