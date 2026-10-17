//! `MemoryBroker` semantics: consumer groups, ack/nack, redelivery, dead-lettering, prefetch and
//! bounded concurrency (msg.brokers #4 #5 #6).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use id_effect::{Cause, Effect, Exit, Never, Schema, Stream, fail, succeed, suspend, yield_fiber};
use id_effect_messaging::{
  BrokerError, ConsumerGroup, DeadLetterReason, Delivery, MemoryBroker, Message, MessageBroker,
  MessageBrokerExt, Nack, StartFrom, SubscribeOptions, Topic, ack_on_success, headers,
};

fn run<A: Send + 'static>(eff: Effect<A, BrokerError>) -> Result<A, BrokerError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn topic(name: &str) -> Topic {
  Topic::new(name).unwrap()
}

fn group(name: &str) -> ConsumerGroup {
  ConsumerGroup::new(name).unwrap()
}

fn publish_all(broker: &MemoryBroker, t: &Topic, payloads: &[&str]) {
  for p in payloads {
    run(broker.publish(t, Message::new(p.as_bytes()))).unwrap();
  }
}

fn subscribe(
  broker: &MemoryBroker,
  t: &Topic,
  g: &ConsumerGroup,
  opts: SubscribeOptions,
) -> Stream<Delivery, BrokerError> {
  run(broker.subscribe(t, g, opts)).unwrap()
}

/// The next chunk of at most `max` deliveries, and the rest of the stream.
fn next(
  stream: Stream<Delivery, BrokerError>,
  max: usize,
) -> (Vec<Delivery>, Stream<Delivery, BrokerError>) {
  let (chunk, rest) = run(stream.next_chunk_with_size(max))
    .unwrap()
    .expect("a chunk");
  (chunk.into_vec(), rest)
}

fn texts(deliveries: &[Delivery]) -> Vec<String> {
  deliveries
    .iter()
    .map(|d| d.message().text().unwrap().to_owned())
    .collect()
}

#[test]
fn nack_redelivers_then_dead_letters() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  run(broker.publish(&t, Message::new("poison").with_id("m-1").with_key("acct-1"))).unwrap();
  let mut stream = subscribe(
    &broker,
    &t,
    &g,
    SubscribeOptions::default().max_deliveries(3),
  );
  for attempt in 1..=3 {
    let (mut chunk, rest) = next(stream, 8);
    stream = rest;
    let d = chunk.pop().unwrap();
    assert!(chunk.is_empty());
    assert_eq!((d.attempt(), d.is_redelivery()), (attempt, attempt > 1));
    assert_eq!(d.message().id().as_str(), "m-1");
    run(d.nack(Nack::Requeue)).unwrap();
  }
  // The third nack hit max_deliveries: dead-lettered, nothing left for the group.
  assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (0, 0));
  let dlq = broker.messages(&topic("orders.dlq"));
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
  drop(stream);
}

#[test]
fn nack_dead_letter_skips_the_retries() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  publish_all(&broker, &t, &["bad", "good"]);
  let poison = topic("orders.poison");
  let opts = SubscribeOptions::default().dead_letter(poison.clone());
  let (mut chunk, _rest) = next(subscribe(&broker, &t, &g, opts), 8);
  let good = chunk.pop().unwrap();
  let bad = chunk.pop().unwrap();
  run(bad.nack(Nack::DeadLetter)).unwrap();
  run(good.ack()).unwrap();
  let dead = broker.messages(&poison);
  assert_eq!(texts_of(&dead), ["bad"]);
  assert_eq!(
    dead[0].header(headers::DEAD_LETTER_REASON),
    Some("rejected")
  );
  assert_eq!(dead[0].header(headers::DELIVERY_COUNT), Some("1"));
  assert!(broker.messages(&topic("orders.dlq")).is_empty());
  assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (0, 0));
}

fn texts_of(messages: &[Message]) -> Vec<String> {
  messages
    .iter()
    .map(|m| m.text().unwrap().to_owned())
    .collect()
}

#[test]
fn dropped_delivery_is_redelivered_first() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  publish_all(&broker, &t, &["a", "b"]);
  let stream = subscribe(&broker, &t, &g, SubscribeOptions::default());
  let (mut chunk, stream) = next(stream, 1);
  drop(chunk.pop());
  // A dropped delivery is a requeue, served before the next new message.
  let (chunk, _stream) = next(stream, 8);
  assert_eq!(texts(&chunk), ["a", "b"]);
  assert_eq!(
    chunk.iter().map(Delivery::attempt).collect::<Vec<_>>(),
    [2, 1]
  );
}

#[test]
fn prefetch_bounds_in_flight() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  let payloads: Vec<String> = (0..10).map(|i| format!("m{i}")).collect();
  publish_all(
    &broker,
    &t,
    &payloads.iter().map(String::as_str).collect::<Vec<_>>(),
  );
  let stream = subscribe(&broker, &t, &g, SubscribeOptions::default().prefetch(3));
  // Asked for 8, handed 3: the prefetch credit.
  let (mut held, stream) = next(stream, 8);
  assert_eq!(texts(&held), ["m0", "m1", "m2"]);
  assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (3, 7));
  // Settling one frees one unit of credit.
  run(held.remove(0).ack()).unwrap();
  let (more, stream) = next(stream, 8);
  assert_eq!(texts(&more), ["m3"]);
  assert_eq!(broker.in_flight(&t, &g), 3);
  drop((held, more, stream));

  // Under `consume`, the unsettled count never exceeds prefetch, nor the running handlers
  // concurrency.
  let broker = MemoryBroker::new();
  publish_all(
    &broker,
    &t,
    &payloads.iter().map(String::as_str).collect::<Vec<_>>(),
  );
  let (running, max_running, max_in_flight, done) = (
    Arc::new(AtomicUsize::new(0)),
    Arc::new(AtomicUsize::new(0)),
    Arc::new(AtomicUsize::new(0)),
    Arc::new(AtomicUsize::new(0)),
  );
  let handler = {
    let (broker, t, g) = (broker.clone(), t.clone(), g.clone());
    let (running, max_running, max_in_flight, done) = (
      Arc::clone(&running),
      Arc::clone(&max_running),
      Arc::clone(&max_in_flight),
      Arc::clone(&done),
    );
    move |d: Delivery| {
      let (broker, t, g) = (broker.clone(), t.clone(), g.clone());
      let (running, max_running, max_in_flight, done) = (
        Arc::clone(&running),
        Arc::clone(&max_running),
        Arc::clone(&max_in_flight),
        Arc::clone(&done),
      );
      // Count when the handler runs, not when `consume` builds its effect.
      suspend(move || {
        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
        max_running.fetch_max(now, Ordering::SeqCst);
        max_in_flight.fetch_max(broker.in_flight(&t, &g), Ordering::SeqCst);
        yield_fiber().flat_map(move |()| {
          running.fetch_sub(1, Ordering::SeqCst);
          if done.fetch_add(1, Ordering::SeqCst) + 1 == 10 {
            broker.close();
          }
          d.ack()
        })
      })
    }
  };
  let opts = SubscribeOptions::default().prefetch(4).concurrency(2);
  run(broker.consume(&t, &g, opts, handler)).unwrap();
  assert_eq!(done.load(Ordering::SeqCst), 10);
  assert!(max_in_flight.load(Ordering::SeqCst) <= 4);
  assert!((1..=2).contains(&max_running.load(Ordering::SeqCst)));
  assert_eq!(broker.backlog(&t, &g), 0);
}

#[test]
fn two_groups_each_get_every_message() {
  let broker = MemoryBroker::new();
  let t = topic("ledger.entry-posted");
  publish_all(&broker, &t, &["e1", "e2", "e3"]);
  for name in ["billing", "audit"] {
    let g = group(name);
    let (chunk, _rest) = next(subscribe(&broker, &t, &g, SubscribeOptions::default()), 8);
    assert_eq!(texts(&chunk), ["e1", "e2", "e3"], "group {name}");
    for d in chunk {
      run(d.ack()).unwrap();
    }
    assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (0, 0));
  }
}

#[test]
fn one_group_shares_messages_between_its_subscriptions() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  publish_all(&broker, &t, &["a", "b", "c", "d"]);
  let opts = SubscribeOptions::default().prefetch(2);
  let (first, _s1) = next(subscribe(&broker, &t, &g, opts.clone()), 8);
  let (second, _s2) = next(subscribe(&broker, &t, &g, opts), 8);
  assert_eq!(texts(&first), ["a", "b"]);
  assert_eq!(texts(&second), ["c", "d"]);
  assert_eq!(broker.in_flight(&t, &g), 4);
}

#[test]
fn a_new_group_can_start_at_the_latest_message() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("late"));
  publish_all(&broker, &t, &["old"]);
  let stream = subscribe(
    &broker,
    &t,
    &g,
    SubscribeOptions::default().start(StartFrom::Latest),
  );
  assert_eq!(broker.backlog(&t, &g), 0);
  publish_all(&broker, &t, &["new"]);
  let (chunk, _rest) = next(stream, 8);
  assert_eq!(texts(&chunk), ["new"]);
  // The group exists now: Earliest on a later subscription does not rewind it.
  let again = subscribe(&broker, &t, &g, SubscribeOptions::default());
  drop(again);
  assert_eq!(broker.backlog(&t, &g), 0);
}

#[test]
fn a_waiting_subscription_wakes_on_publish() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  let stream = subscribe(&broker, &t, &g, SubscribeOptions::default());
  let waiter = stream
    .take(1)
    .map_effect(|d: Delivery| {
      let text = d.message().text().unwrap().to_owned();
      d.ack().map(move |()| text)
    })
    .run_collect()
    .fork();
  let publisher = broker.clone();
  let eff = waiter
    .map_err(|n: Never| match n {})
    .flat_map(move |handle| {
      publisher
        .publish(&t, Message::new("late"))
        .flat_map(move |()| handle.join())
    });
  assert_eq!(run(eff), Ok(vec!["late".to_owned()]));
}

#[test]
fn close_ends_streams_and_rejects_publishes() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  publish_all(&broker, &t, &["a"]);
  let stream = subscribe(&broker, &t, &g, SubscribeOptions::default());
  let (mut chunk, stream) = next(stream, 8);
  broker.close();
  assert_eq!(run(stream.run_collect()).map(|v| v.len()), Ok(0));
  // Settling still works after close.
  run(chunk.pop().unwrap().ack()).unwrap();
  assert_eq!(
    run(broker.publish(&t, Message::new("b"))),
    Err(BrokerError::Closed)
  );
  assert!(matches!(
    run(broker.subscribe(&t, &g, SubscribeOptions::default())),
    Err(BrokerError::Closed)
  ));
}

#[test]
fn an_outage_fails_operations_until_it_recovers() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  let stream = subscribe(&broker, &t, &g, SubscribeOptions::default());
  broker.fail_with(Some("down"));
  let down = Err(BrokerError::Connection("down".into()));
  assert_eq!(run(broker.publish(&t, Message::new("a"))), down);
  assert!(matches!(
    run(broker.subscribe(&t, &g, SubscribeOptions::default())),
    Err(BrokerError::Connection(_))
  ));
  assert_eq!(run(stream.run_collect()).map(|v| v.len()), down.map(|()| 0));
  broker.fail_with(None);
  publish_all(&broker, &t, &["a"]);
  let (chunk, _rest) = next(subscribe(&broker, &t, &g, SubscribeOptions::default()), 8);
  assert_eq!(texts(&chunk), ["a"]);
}

#[test]
fn invalid_options_fail_the_subscribe() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  for opts in [
    SubscribeOptions::default().prefetch(0),
    SubscribeOptions::default().prefetch(1).concurrency(2),
    SubscribeOptions::default().dead_letter(t.clone()),
  ] {
    assert!(matches!(
      run(broker.subscribe(&t, &g, opts)),
      Err(BrokerError::InvalidOptions(_))
    ));
  }
}

#[derive(Clone, Debug, PartialEq, Schema)]
struct EntryPosted {
  account: String,
  cents: i64,
}

#[test]
fn typed_payloads_round_trip_through_the_schema() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("ledger.entry-posted"), group("billing"));
  let entry = EntryPosted {
    account: "acct-1".into(),
    cents: 1250,
  };
  run(broker.publish_encoded(&t, entry.clone())).unwrap();
  run(broker.publish(&t, Message::new(r#"{"account": 7}"#))).unwrap();
  let (chunk, _rest) = next(subscribe(&broker, &t, &g, SubscribeOptions::default()), 8);
  assert_eq!(
    chunk[0].message().header(headers::CONTENT_TYPE),
    Some(headers::JSON)
  );
  assert_eq!(chunk[0].decode::<EntryPosted>(), Ok(entry));
  let Err(BrokerError::Decode(why)) = chunk[1].decode::<EntryPosted>() else {
    panic!("expected a decode error");
  };
  assert!(why.contains("account") && why.contains("cents"), "{why}");
  assert!(matches!(
    Message::new("not json").decode::<EntryPosted>(),
    Err(BrokerError::Decode(_))
  ));
}

#[test]
fn ack_on_success_retries_failures_until_they_dead_letter() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  publish_all(&broker, &t, &["ok", "boom"]);
  let seen = Arc::new(AtomicUsize::new(0));
  let handler = {
    let (broker, seen) = (broker.clone(), Arc::clone(&seen));
    ack_on_success(move |d: &Delivery| -> Effect<(), String> {
      // "ok" once, then "boom" twice (max_deliveries 2): three deliveries in all.
      if seen.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
        broker.close();
      }
      if d.message().payload() == b"boom" {
        fail("boom".into())
      } else {
        succeed(())
      }
    })
  };
  let opts = SubscribeOptions::default().max_deliveries(2);
  run(broker.consume(&t, &g, opts, handler)).unwrap();
  assert_eq!(seen.load(Ordering::SeqCst), 3);
  assert_eq!(texts_of(&broker.messages(&topic("orders.dlq"))), ["boom"]);
  assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (0, 0));
}

#[test]
fn a_failing_handler_ends_consume_and_requeues_its_delivery() {
  let broker = MemoryBroker::new();
  let (t, g) = (topic("orders"), group("billing"));
  publish_all(&broker, &t, &["a"]);
  let out = run(
    broker.consume(&t, &g, SubscribeOptions::default(), |_d: Delivery| {
      fail(BrokerError::Backend("handler gave up".into()))
    }),
  );
  assert_eq!(out, Err(BrokerError::Backend("handler gave up".into())));
  assert_eq!((broker.in_flight(&t, &g), broker.backlog(&t, &g)), (0, 1));
}
