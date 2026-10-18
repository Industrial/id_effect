//! Balance notifications (msg.brokers #3 #5 #6 #7 #8): a posting writes its announcement to the
//! outbox in its own `SERIALIZABLE` transaction, the relay publishes the committed rows to Redis
//! Streams, and the consumer group writes one balance notification per account with the inbox
//! row, so a redelivered message notifies once; a message it cannot read is dead-lettered.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{run, with_ledger};
use id_effect::{Effect, HNil};
use id_effect_kit::messaging::{
  ConsumerGroup, Message, MessageBroker, SubscribeOptions, Topic, headers,
};
use id_effect_kit::redis::{
  RedisClient, RedisConfig, RedisStreamsBroker, RedisStreamsConfig, redis,
};
use id_effect_kit::sql::params;
use id_effect_kit::test::http::StatusCode;
use id_effect_kit::test::{ContainerError, ContainerImage, Redis, RedisContainer};
use ledger::notifications::{self, EntryPosted};
use serde_json::{Value, json};

/// A Redis server and a broker on it that redelivers quickly.
async fn broker() -> Option<(RedisContainer, RedisClient, Arc<dyn MessageBroker>)> {
  if !id_effect_kit::test::require_docker() {
    return None;
  }
  let container = run(Redis::new().start::<ContainerError, HNil>()).await;
  let client = run(RedisClient::connect(RedisConfig::new(container.url()))).await;
  let config = RedisStreamsConfig::new()
    .consumer("ledger-test")
    .min_idle(Duration::from_millis(300))
    .block(Duration::from_millis(100));
  let broker: Arc<dyn MessageBroker> = Arc::new(RedisStreamsBroker::new(client.clone(), config));
  Some((container, client, broker))
}

/// Runs the consumer `handler` of `group` over the next `n` deliveries.
fn consume(
  broker: &Arc<dyn MessageBroker>,
  topic: &Topic,
  group: &ConsumerGroup,
  options: SubscribeOptions,
  db: Arc<dyn id_effect_kit::sql::SqlClient>,
  n: usize,
) -> Effect<(), id_effect_kit::messaging::BrokerError> {
  let handler = notifications::handler(db);
  broker
    .subscribe(topic, group, options)
    .flat_map(move |deliveries| deliveries.take(n).run_for_each(handler))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn postings_reach_the_balance_consumer_through_the_outbox_once() {
  let Some((_container, _client, broker)) = broker().await else {
    return;
  };
  with_ledger(
    |_, _| {},
    move |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 3).await;
      let (cash, sales, bank) = (accounts[0].id, accounts[1].id, accounts[2].id);
      let entry = l.transfer(&team.accountant, cash, sales, 300).await;

      // Staged with the entry: the outbox row commits with it.
      let staged = l
        .scalar(
          "SELECT count(*)::int8 FROM id_effect_outbox \
           WHERE message_id = $1 AND sent_at IS NULL AND topic = 'ledger.entry-posted'",
          params![format!("entry-{entry}")],
        )
        .await;
      assert_eq!(staged, 1);

      // A batch whose second entry the database refuses (a closed account): its savepoint rolls
      // back, announcement included.
      l.execute(
        "UPDATE accounts SET closed_at = now() WHERE id = $1",
        params![bank],
      )
      .await;
      let batch = l
        .post(
          "/entries/batch",
          json!([
            {"memo": "second", "lines": [
              {"account_id": cash, "amount": 20}, {"account_id": sales, "amount": -20}]},
            {"memo": "refused", "lines": [
              {"account_id": bank, "amount": 5}, {"account_id": sales, "amount": -5}]},
          ]),
          &team.accountant,
        )
        .await;
      batch.assert_status(StatusCode::OK);
      let lines: Value = batch.json().unwrap();
      assert!(lines["entries"][1]["error"].is_string(), "{lines}");
      assert_eq!(
        l.scalar("SELECT count(*)::int8 FROM id_effect_outbox", vec![])
          .await,
        2,
        "the refused entry announced nothing"
      );

      // The relay publishes what committed, then the consumer group notifies.
      let report = run(notifications::relay(&l.db, Arc::clone(&broker)).run_until_idle()).await;
      assert_eq!(report.published, 2);
      let (topic, group) = notifications::names(&l.config.messaging).unwrap();
      let options = notifications::options(&l.config.messaging);
      run(consume(
        &broker,
        &topic,
        &group,
        options.clone(),
        Arc::clone(&l.db),
        2,
      ))
      .await;
      let notified = |account: i64| {
        l.scalar(
          "SELECT COALESCE(max(balance), 0)::int8 FROM balance_notifications \
           WHERE account_id = $1",
          params![account],
        )
      };
      assert_eq!(notified(cash).await, 320);
      assert_eq!(notified(sales).await, -320);
      let rows = "SELECT count(*)::int8 FROM balance_notifications";
      assert_eq!(l.scalar(rows, vec![]).await, 4);

      // The same announcement again (a relay that crashed after publishing): the inbox has it,
      // nothing is notified twice.
      let again = Message::encode(EntryPosted {
        org_id: "acme".into(),
        entry_id: entry,
        account_ids: vec![cash, sales],
      })
      .with_id(format!("entry-{entry}"));
      run(broker.publish(&topic, again)).await;
      run(consume(
        &broker,
        &topic,
        &group,
        options,
        Arc::clone(&l.db),
        1,
      ))
      .await;
      assert_eq!(l.scalar(rows, vec![]).await, 4);
      assert_eq!(
        l.scalar(
          "SELECT count(*)::int8 FROM id_effect_inbox WHERE consumer_group = $1",
          params![group.as_str()],
        )
        .await,
        2
      );
    },
  )
  .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreadable_announcement_is_dead_lettered() {
  let Some((_container, client, broker)) = broker().await else {
    return;
  };
  with_ledger(
    |_, _| {},
    move |l| async move {
      let (topic, group) = notifications::names(&l.config.messaging).unwrap();
      let options = notifications::options(&l.config.messaging).max_deliveries(2);
      run(broker.publish(&topic, Message::new("not an EntryPosted").with_id("bad-1"))).await;
      // Two deliveries, both failing: the second is the last, so the message moves to the DLQ.
      run(consume(
        &broker,
        &topic,
        &group,
        options,
        Arc::clone(&l.db),
        2,
      ))
      .await;

      let dlq = topic.dead_letter().unwrap();
      let entries: Vec<redis::Value> = run(
        client.query(
          redis::cmd("XRANGE")
            .arg(dlq.as_str())
            .arg("-")
            .arg("+")
            .clone(),
        ),
      )
      .await;
      assert_eq!(entries.len(), 1, "one dead letter");
      let dead = run(
        broker
          .subscribe(
            &dlq,
            &ConsumerGroup::new("ops").unwrap(),
            SubscribeOptions::default(),
          )
          .flat_map(|deliveries| deliveries.take(1).run_collect()),
      )
      .await;
      let message = dead[0].message();
      assert_eq!(message.id().as_str(), "bad-1");
      assert_eq!(
        message.header(headers::DEAD_LETTER_GROUP),
        Some(group.as_str())
      );
      assert_eq!(message.header(headers::DELIVERY_COUNT), Some("2"));
      // Nothing was notified, and the inbox holds no record of it (it may be replayed).
      assert_eq!(
        l.scalar("SELECT count(*)::int8 FROM id_effect_inbox", vec![])
          .await,
        0
      );
    },
  )
  .await;
}
