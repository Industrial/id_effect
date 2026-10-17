//! `RedisStreamsBroker` against a Redis container: the `MemoryBroker` scenarios of
//! `crates/id_effect_messaging/tests/memory_broker.rs` (consumer groups, ack/nack, redelivery,
//! dead-lettering, prefetch, bounded concurrency) on Redis Streams, then what is specific to
//! Redis (idle reclaim, the stream entries, the outbox relay end to end). msg.brokers #3 #5 #6.

#![cfg(feature = "streams")]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{ok, redis_client, run};
use id_effect::{Effect, Never, Schema, Stream, fail, succeed, suspend, yield_fiber};
use id_effect_messaging::{
  BrokerError, ConsumerGroup, DeadLetterReason, Delivery, MemoryInbox, MemoryOutbox, Message,
  MessageBroker, MessageBrokerExt, Nack, Relay, StartFrom, SubscribeOptions, Topic, ack_on_success,
  headers,
};
use id_effect_redis::{RedisClient, RedisConfig, RedisStreamsBroker, RedisStreamsConfig};
use redis::streams::StreamInfoGroupsReply;

fn topic(name: &str) -> Topic {
  Topic::new(name).unwrap()
}

fn group(name: &str) -> ConsumerGroup {
  ConsumerGroup::new(name).unwrap()
}

/// Short timeouts, so redelivery and close are quick.
fn broker(redis: &RedisClient) -> RedisStreamsBroker {
  RedisStreamsBroker::new(
    redis.clone(),
    RedisStreamsConfig::new()
      .consumer("test-1")
      .min_idle(Duration::from_millis(300))
      .block(Duration::from_millis(100)),
  )
}

async fn publish_all(broker: &RedisStreamsBroker, t: &Topic, payloads: &[&str]) {
  for p in payloads {
    ok(broker.publish(t, Message::new(p.as_bytes()))).await;
  }
}

async fn subscribe(
  broker: &RedisStreamsBroker,
  t: &Topic,
  g: &ConsumerGroup,
  opts: SubscribeOptions,
) -> Stream<Delivery, BrokerError> {
  ok(broker.subscribe(t, g, opts)).await
}

/// The next chunk of at most `max` deliveries, and the rest of the stream.
async fn next(
  stream: Stream<Delivery, BrokerError>,
  max: usize,
) -> (Vec<Delivery>, Stream<Delivery, BrokerError>) {
  let (chunk, rest) = ok(stream.next_chunk_with_size(max)).await.expect("a chunk");
  (chunk.into_vec(), rest)
}

fn texts(deliveries: &[Delivery]) -> Vec<String> {
  deliveries
    .iter()
    .map(|d| d.message().text().unwrap().to_owned())
    .collect()
}

fn attempts(deliveries: &[Delivery]) -> Vec<u32> {
  deliveries.iter().map(Delivery::attempt).collect()
}

/// The entries of `group` on `t` delivered and not acked (`XPENDING`).
async fn pending(redis: &RedisClient, t: &Topic, g: &ConsumerGroup) -> u64 {
  let cmd = redis::cmd("XPENDING")
    .arg(t.as_str())
    .arg(g.as_str())
    .clone();
  let (count, ..): (u64, redis::Value, redis::Value, redis::Value) = ok(redis.query(cmd)).await;
  count
}

/// The entries of `t` that `g` has not read yet (`XINFO GROUPS` lag).
async fn lag(redis: &RedisClient, t: &Topic, g: &ConsumerGroup) -> usize {
  let cmd = redis::cmd("XINFO").arg("GROUPS").arg(t.as_str()).clone();
  let reply: StreamInfoGroupsReply = ok(redis.query(cmd)).await;
  let info = reply.groups.into_iter().find(|i| i.name == g.as_str());
  info.and_then(|i| i.lag).expect("group lag")
}

/// Every message of the stream `t`, read with a fresh group.
async fn messages(redis: &RedisClient, t: &Topic) -> Vec<Message> {
  let b = broker(redis);
  let stream = subscribe(&b, t, &group("inspect"), SubscribeOptions::default()).await;
  let mut out = Vec::new();
  let mut stream = stream;
  loop {
    let eff = stream.next_chunk_with_size(64);
    // A short wait for the first chunk: one block time.
    let pulled = run(eff.timeout(Duration::from_millis(400))).await.unwrap();
    let Some(Some((chunk, rest))) = pulled else {
      break;
    };
    for d in chunk.into_vec() {
      out.push(d.message().clone());
      ok(d.ack()).await;
    }
    stream = rest;
  }
  out
}

fn texts_of(messages: &[Message]) -> Vec<String> {
  messages
    .iter()
    .map(|m| m.text().unwrap().to_owned())
    .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consumer_group_ack_and_redelivery() {
  let Some((_redis_container, redis)) = redis_client("consumer_group_ack_and_redelivery").await
  else {
    return;
  };
  let b = broker(&redis);
  let t = topic("ledger.entry-posted");
  let sent = Message::new(vec![0xff, 0x00, b'x'])
    .with_id("m-1")
    .with_key("acct-1")
    .with_header(headers::CONTENT_TYPE, "application/octet-stream")
    .with_header("trace", "t-1");
  ok(b.publish(&t, sent.clone())).await;
  publish_all(&b, &t, &["e2", "e3"]).await;

  // Every group gets every message; the id, key, headers and raw payload survive the stream.
  for name in ["audit", "billing"] {
    let g = group(name);
    let (chunk, _rest) = next(subscribe(&b, &t, &g, SubscribeOptions::default()).await, 8).await;
    assert_eq!(chunk.len(), 3, "group {name}");
    assert_eq!(chunk[0].message(), &sent);
    assert_eq!(attempts(&chunk), [1, 1, 1]);
    assert_eq!((chunk[0].topic(), chunk[0].group()), (&t, &g));
    if name == "audit" {
      for d in chunk {
        ok(d.ack()).await;
      }
      assert_eq!(pending(&redis, &t, &g).await, 0);
      continue;
    }
    // billing: ack one, requeue one, drop one.
    let mut chunk = chunk.into_iter();
    let (first, second, third) = (
      chunk.next().unwrap(),
      chunk.next().unwrap(),
      chunk.next().unwrap(),
    );
    ok(first.ack()).await;
    ok(second.nack(Nack::Requeue)).await;
    drop(third);
    assert_eq!(pending(&redis, &t, &g).await, 2);
  }

  // A new subscription of billing: the requeued entry right away, the dropped one once idle.
  let g = group("billing");
  let stream = subscribe(&b, &t, &g, SubscribeOptions::default()).await;
  let (chunk, stream) = next(stream, 8).await;
  assert_eq!(
    (texts(&chunk), attempts(&chunk)),
    (vec!["e2".to_owned()], vec![2])
  );
  assert!(chunk[0].is_redelivery());
  let (again, _stream) = next(stream, 8).await;
  assert_eq!(
    (texts(&again), attempts(&again)),
    (vec!["e3".to_owned()], vec![2])
  );
  for d in chunk.into_iter().chain(again) {
    ok(d.ack()).await;
  }
  assert_eq!(
    (pending(&redis, &t, &g).await, lag(&redis, &t, &g).await),
    (0, 0)
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poison_message_moves_to_dlq() {
  let Some((_redis_container, redis)) = redis_client("poison_message_moves_to_dlq").await else {
    return;
  };
  let b = broker(&redis);
  let (t, g) = (topic("orders"), group("billing"));
  ok(b.publish(&t, Message::new("poison").with_id("m-1").with_key("acct-1"))).await;
  let mut stream = subscribe(&b, &t, &g, SubscribeOptions::default().max_deliveries(3)).await;
  for attempt in 1..=3 {
    let (mut chunk, rest) = next(stream, 8).await;
    stream = rest;
    let d = chunk.pop().unwrap();
    assert!(chunk.is_empty());
    assert_eq!((d.attempt(), d.message().id().as_str()), (attempt, "m-1"));
    ok(d.nack(Nack::Requeue)).await;
  }
  drop(stream);
  // The third nack hit max_deliveries: acked in the group, added to orders.dlq.
  assert_eq!(
    (pending(&redis, &t, &g).await, lag(&redis, &t, &g).await),
    (0, 0)
  );
  let dlq = messages(&redis, &topic("orders.dlq")).await;
  assert_eq!(dlq.len(), 1);
  let dead = &dlq[0];
  assert_eq!(
    (dead.id().as_str(), dead.key(), dead.payload()),
    ("m-1", Some("acct-1"), &b"poison"[..])
  );
  assert_eq!(dead.header(headers::DEAD_LETTER_TOPIC), Some("orders"));
  assert_eq!(dead.header(headers::DEAD_LETTER_GROUP), Some("billing"));
  assert_eq!(
    dead.header(headers::DEAD_LETTER_REASON),
    Some(DeadLetterReason::MaxDeliveries.as_str())
  );
  assert_eq!(dead.header(headers::DELIVERY_COUNT), Some("3"));

  // Nack::DeadLetter skips the retries, to the configured dead-letter topic.
  let (t, poison) = (topic("payments"), topic("payments.poison"));
  publish_all(&b, &t, &["bad", "good"]).await;
  let opts = SubscribeOptions::default().dead_letter(poison.clone());
  let (mut chunk, _rest) = next(subscribe(&b, &t, &g, opts).await, 8).await;
  let good = chunk.pop().unwrap();
  ok(chunk.pop().unwrap().nack(Nack::DeadLetter)).await;
  ok(good.ack()).await;
  let dead = messages(&redis, &poison).await;
  assert_eq!(texts_of(&dead), ["bad"]);
  assert_eq!(
    dead[0].header(headers::DEAD_LETTER_REASON),
    Some("rejected")
  );
  assert_eq!(dead[0].header(headers::DELIVERY_COUNT), Some("1"));
  assert_eq!(pending(&redis, &t, &g).await, 0);

  // A delivery dropped on its last attempt is dead-lettered when it is reclaimed.
  let t = topic("refunds");
  publish_all(&b, &t, &["lost"]).await;
  let opts = SubscribeOptions::default().max_deliveries(1);
  let (chunk, stream) = next(subscribe(&b, &t, &g, opts).await, 8).await;
  drop(chunk);
  let waited = run(
    stream
      .next_chunk_with_size(8)
      .timeout(Duration::from_millis(900)),
  )
  .await;
  assert!(matches!(waited, Ok(None)), "nothing to redeliver");
  let dead = messages(&redis, &topic("refunds.dlq")).await;
  assert_eq!(texts_of(&dead), ["lost"]);
  assert_eq!(dead[0].header(headers::DELIVERY_COUNT), Some("1"));
  assert_eq!(pending(&redis, &t, &g).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prefetch_bounds_in_flight() {
  let Some((_redis_container, redis)) = redis_client("prefetch_bounds_in_flight").await else {
    return;
  };
  let b = broker(&redis);
  let (t, g) = (topic("orders"), group("billing"));
  let payloads: Vec<String> = (0..10).map(|i| format!("m{i}")).collect();
  let payloads: Vec<&str> = payloads.iter().map(String::as_str).collect();
  publish_all(&b, &t, &payloads).await;
  let stream = subscribe(&b, &t, &g, SubscribeOptions::default().prefetch(3)).await;
  // Asked for 8, handed 3: the prefetch credit (XREADGROUP COUNT 3).
  let (mut held, stream) = next(stream, 8).await;
  assert_eq!(texts(&held), ["m0", "m1", "m2"]);
  assert_eq!(
    (pending(&redis, &t, &g).await, lag(&redis, &t, &g).await),
    (3, 7)
  );
  // Settling one frees one unit of credit.
  ok(held.remove(0).ack()).await;
  let (more, stream) = next(stream, 8).await;
  assert_eq!(texts(&more), ["m3"]);
  // Out of credit, the pull waits for a settlement, not for Redis.
  let waiting = run(
    stream
      .next_chunk_with_size(8)
      .timeout(Duration::from_millis(300)),
  )
  .await;
  assert!(matches!(waiting, Ok(None)));
  assert_eq!(pending(&redis, &t, &g).await, 3);
  for d in held.into_iter().chain(more) {
    ok(d.ack()).await;
  }

  // Under `consume`, the unsettled entries never exceed prefetch, nor the running handlers
  // concurrency.
  let (running, max_running, max_pending, done) = (
    Arc::new(AtomicUsize::new(0)),
    Arc::new(AtomicUsize::new(0)),
    Arc::new(AtomicUsize::new(0)),
    Arc::new(AtomicUsize::new(0)),
  );
  let handler = {
    let (b, redis, t, g) = (b.clone(), redis.clone(), t.clone(), g.clone());
    let (running, max_running, max_pending, done) = (
      Arc::clone(&running),
      Arc::clone(&max_running),
      Arc::clone(&max_pending),
      Arc::clone(&done),
    );
    move |d: Delivery| {
      let (b, redis, t, g) = (b.clone(), redis.clone(), t.clone(), g.clone());
      let (running, max_running, max_pending, done) = (
        Arc::clone(&running),
        Arc::clone(&max_running),
        Arc::clone(&max_pending),
        Arc::clone(&done),
      );
      suspend(move || {
        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
        max_running.fetch_max(now, Ordering::SeqCst);
        let cmd = redis::cmd("XPENDING")
          .arg(t.as_str())
          .arg(g.as_str())
          .clone();
        redis
          .query::<(u64, redis::Value, redis::Value, redis::Value)>(cmd)
          .map_err(BrokerError::from)
          .flat_map(move |(count, ..)| {
            max_pending.fetch_max(usize::try_from(count).unwrap(), Ordering::SeqCst);
            yield_fiber().flat_map(move |()| {
              running.fetch_sub(1, Ordering::SeqCst);
              // m4..m9: six more.
              if done.fetch_add(1, Ordering::SeqCst) + 1 == 6 {
                b.close();
              }
              d.ack()
            })
          })
      })
    }
  };
  let opts = SubscribeOptions::default().prefetch(4).concurrency(2);
  ok(b.consume(&t, &g, opts, handler)).await;
  assert_eq!(done.load(Ordering::SeqCst), 6);
  assert!(max_pending.load(Ordering::SeqCst) <= 4);
  assert!((1..=2).contains(&max_running.load(Ordering::SeqCst)));
  assert_eq!(
    (pending(&redis, &t, &g).await, lag(&redis, &t, &g).await),
    (0, 0)
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbox_relay_to_streams_end_to_end() {
  let Some((_redis_container, redis)) = redis_client("outbox_relay_to_streams_end_to_end").await
  else {
    return;
  };
  let b = broker(&redis);
  let orders = topic("orders");
  let outbox = MemoryOutbox::new();
  for id in ["o-1", "o-2", "o-3"] {
    ok(outbox.stage(&orders, Message::new(id).with_id(id))).await;
  }
  let relay = Relay::new(Arc::new(outbox.clone()), Arc::new(b.clone()));
  let report = ok(relay.run_until_idle()).await;
  assert_eq!((report.claimed, report.published), (3, 3));
  assert_eq!(outbox.sent(), 3);
  // At least once: a relay that crashed after publishing o-2 publishes it again.
  ok(b.publish(&orders, Message::new("o-2").with_id("o-2"))).await;

  // The inbox processes each message id once; every delivery is acked.
  let inbox = MemoryInbox::new();
  let processed = Arc::new(std::sync::Mutex::new(Vec::new()));
  let handler = inbox.handler({
    let processed = Arc::clone(&processed);
    move |m: &Message| {
      processed.lock().unwrap().push(m.id().as_str().to_owned());
      succeed::<(), (), _>(())
    }
  });
  let g = group("billing");
  let stream = subscribe(&b, &orders, &g, SubscribeOptions::default()).await;
  ok(stream.take(4).run_for_each(handler)).await;
  assert_eq!(*processed.lock().unwrap(), ["o-1", "o-2", "o-3"]);
  assert_eq!(
    (
      pending(&redis, &orders, &g).await,
      lag(&redis, &orders, &g).await
    ),
    (0, 0)
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriptions_share_a_group_and_new_groups_start_where_asked() {
  let Some((_redis_container, redis)) = redis_client("subscriptions_share_a_group").await else {
    return;
  };
  let b = broker(&redis);
  let (t, g) = (topic("orders"), group("billing"));
  publish_all(&b, &t, &["a", "b", "c", "d"]).await;
  let opts = SubscribeOptions::default().prefetch(2);
  let (first, _s1) = next(subscribe(&b, &t, &g, opts.clone()).await, 8).await;
  let (second, _s2) = next(subscribe(&b, &t, &g, opts).await, 8).await;
  assert_eq!(texts(&first), ["a", "b"]);
  assert_eq!(texts(&second), ["c", "d"]);
  assert_eq!(pending(&redis, &t, &g).await, 4);

  // StartFrom::Latest skips what is already there; the group then keeps its position.
  let late = group("late");
  let stream = subscribe(
    &b,
    &t,
    &late,
    SubscribeOptions::default().start(StartFrom::Latest),
  )
  .await;
  assert_eq!(lag(&redis, &t, &late).await, 0);
  publish_all(&b, &t, &["new"]).await;
  let (chunk, _rest) = next(stream, 8).await;
  assert_eq!(texts(&chunk), ["new"]);
  drop(subscribe(&b, &t, &late, SubscribeOptions::default()).await);
  assert_eq!(lag(&redis, &t, &late).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_waiting_subscription_wakes_on_publish_and_close_ends_it() {
  let Some((_redis_container, redis)) = redis_client("a_waiting_subscription_wakes").await else {
    return;
  };
  let b = broker(&redis);
  let (t, g) = (topic("orders"), group("billing"));
  let stream = subscribe(&b, &t, &g, SubscribeOptions::default()).await;
  let waiter = stream
    .take(1)
    .map_effect(|d: Delivery| {
      let text = d.message().text().unwrap().to_owned();
      d.ack().map(move |()| text)
    })
    .run_collect()
    .fork();
  let publisher = b.clone();
  let t2 = t.clone();
  let eff = waiter
    .map_err(|n: Never| match n {})
    .flat_map(move |handle| {
      publisher
        .publish(&t2, Message::new("late"))
        .flat_map(move |()| handle.join())
    });
  assert_eq!(run(eff).await, Ok(vec!["late".to_owned()]));

  // close: the stream ends (within a block time), settling still works, publish and subscribe
  // fail.
  publish_all(&b, &t, &["a"]).await;
  let stream = subscribe(&b, &t, &g, SubscribeOptions::default()).await;
  let (mut chunk, stream) = next(stream, 8).await;
  b.close();
  assert_eq!(ok(stream.run_collect()).await.len(), 0);
  ok(chunk.pop().unwrap().ack()).await;
  assert_eq!(pending(&redis, &t, &g).await, 0);
  assert_eq!(
    run(b.publish(&t, Message::new("b"))).await,
    Err(BrokerError::Closed)
  );
  assert!(matches!(
    run(b.subscribe(&t, &g, SubscribeOptions::default())).await,
    Err(BrokerError::Closed)
  ));
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct EntryPosted {
  account: String,
  cents: i64,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_payloads_options_and_failures() {
  let Some((_redis_container, redis)) = redis_client("typed_payloads_options_and_failures").await
  else {
    return;
  };
  let b = broker(&redis);
  let (t, g) = (topic("ledger.entry-posted"), group("billing"));
  let entry = EntryPosted {
    account: "acct-1".into(),
    cents: 1250,
  };
  ok(b.publish_encoded(&t, entry.clone())).await;
  let (chunk, _rest) = next(subscribe(&b, &t, &g, SubscribeOptions::default()).await, 8).await;
  assert_eq!(
    chunk[0].message().header(headers::CONTENT_TYPE),
    Some(headers::JSON)
  );
  assert_eq!(chunk[0].decode::<EntryPosted>(), Ok(entry));

  for opts in [
    SubscribeOptions::default().prefetch(0),
    SubscribeOptions::default().prefetch(1).concurrency(2),
    SubscribeOptions::default().dead_letter(t.clone()),
  ] {
    assert!(matches!(
      run(b.subscribe(&t, &g, opts)).await,
      Err(BrokerError::InvalidOptions(_))
    ));
  }

  // A key of another type: the broker's error, not a panic.
  ok(redis.query::<String>(redis::cmd("SET").arg("plain").arg("x").clone())).await;
  let plain = topic("plain");
  assert!(matches!(
    run(b.publish(&plain, Message::new("m"))).await,
    Err(BrokerError::Backend(_))
  ));
  assert!(matches!(
    run(b.subscribe(&plain, &g, SubscribeOptions::default())).await,
    Err(BrokerError::Backend(_))
  ));

  // A failing handler under ack_on_success is retried, then dead-lettered.
  let jobs = topic("jobs");
  publish_all(&b, &jobs, &["ok", "boom"]).await;
  let seen = Arc::new(AtomicUsize::new(0));
  let handler = {
    let (b, seen) = (b.clone(), Arc::clone(&seen));
    ack_on_success(move |d: &Delivery| -> Effect<(), String> {
      if seen.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
        b.close();
      }
      if d.message().payload() == b"boom" {
        fail("boom".into())
      } else {
        succeed(())
      }
    })
  };
  let opts = SubscribeOptions::default().max_deliveries(2);
  ok(b.consume(&jobs, &g, opts, handler)).await;
  assert_eq!(seen.load(Ordering::SeqCst), 3);
  assert_eq!(
    texts_of(&messages(&redis, &topic("jobs.dlq")).await),
    ["boom"]
  );
  assert_eq!(pending(&redis, &jobs, &g).await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_server_is_a_connection_error() {
  // Needs no container: nothing listens on port 1.
  let config = RedisConfig::new("redis://127.0.0.1:1")
    .connection_timeout(Duration::from_millis(200))
    .reconnect_retries(0);
  let redis = ok(RedisClient::connect_lazy(config)).await;
  let b = broker(&redis);
  let (t, g) = (topic("orders"), group("billing"));
  assert!(matches!(
    run(b.publish(&t, Message::new("m"))).await,
    Err(BrokerError::Connection(_))
  ));
  assert!(matches!(
    run(b.subscribe(&t, &g, SubscribeOptions::default())).await,
    Err(BrokerError::Connection(_))
  ));
  assert!(format!("{b:?}").contains("closed: false"));
}
