//! The background work of one instance (`jobs.run = true`), each a fiber `main.rs` runs next to
//! the HTTP server until shutdown:
//!
//! | Task | What |
//! |---|---|
//! | `worker` | the `statements` and `mail` queues ([`crate::jobs`]) |
//! | `cron` | the statement cron, on the instance holding the advisory lock |
//! | `relay` | the outbox to Redis Streams ([`crate::notifications`]) |
//! | `notifications` | the balance-notification consumer group |
//! | `limits` | polls the profile file for new login limits ([`crate::limits`]) |
//!
//! Without Redis the announcements stay in the outbox (no relay, no consumer). A consumer whose
//! broker connection fails subscribes again after [`RESUBSCRIBE`].

use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Never, sleep, succeed};
use id_effect_kit::messaging::{ConsumerGroup, MessageBroker, SubscribeOptions, Topic};
use id_effect_kit::sql::SqlClient;

use crate::Services;
use crate::config::LedgerConfig;
use crate::error::StartupError;
use crate::jobs::{self, Statements};
use crate::mail::{self, Mail};
use crate::notifications;

/// How long a failed consumer waits before it subscribes again.
pub const RESUBSCRIBE: Duration = Duration::from_secs(5);

/// One background fiber: its name and its program.
pub type Task = (&'static str, Effect<(), Never>);

/// The tasks of `config` over `services` (none when `jobs.run` is off).
pub fn tasks(config: &LedgerConfig, services: &Services) -> Effect<Vec<Task>, StartupError> {
  if !config.jobs.run {
    return succeed(Vec::new());
  }
  let db = Arc::clone(&services.database.primary);
  let mail = match mail_of(config, services) {
    Ok(mail) => mail,
    Err(e) => return id_effect::fail(e),
  };
  let statements = Statements {
    db: Arc::clone(&db),
    storage: services.storage.clone(),
    mail,
    config: config.statements.clone(),
  };
  let registry = match jobs::registry(statements) {
    Ok(registry) => registry,
    Err(e) => return id_effect::fail(StartupError::Settings(format!("jobs: {e}"))),
  };
  let cron = match jobs::statements_cron(&config.statements.cron) {
    Ok(cron) => cron,
    Err(e) => return id_effect::fail(e),
  };
  let env = jobs::job_env(jobs::queue(&db), services.mailer.clone());
  let worker = jobs::worker(registry, config.jobs.concurrency)
    .run()
    .provide_all(env.clone());
  let mut tasks: Vec<Task> = vec![("worker", worker)];
  if let Some(broker) = services.broker.clone() {
    tasks.push((
      "relay",
      notifications::relay(&db, Arc::clone(&broker)).run(),
    ));
    tasks.push(("notifications", consumer(config, broker, Arc::clone(&db))));
  }
  if let Some(watch) = services.limits.clone() {
    tasks.push(("limits", watch.run()));
  }
  let (url, holder) = (
    config.db.url().expose().clone(),
    config.messaging.consumer.clone(),
  );
  jobs::cron_leader(url, holder).map(move |leader| {
    tasks.push(("cron", jobs::run_cron(&leader, &cron, env)));
    tasks
  })
}

fn mail_of(config: &LedgerConfig, services: &Services) -> Result<Mail, StartupError> {
  Ok(Mail {
    mailer: services.mailer.clone(),
    templates: mail::templates(&config.mail)?,
    from: mail::sender(&config.mail)?,
  })
}

/// The balance-notification consumer, subscribing again whenever it stops.
fn consumer(
  config: &LedgerConfig,
  broker: Arc<dyn MessageBroker>,
  db: Arc<dyn SqlClient>,
) -> Effect<(), Never> {
  let Ok((topic, group)) = notifications::names(&config.messaging) else {
    return succeed(());
  };
  let options = notifications::options(&config.messaging);
  subscribe(broker, db, (topic, group, options))
}

fn subscribe(
  broker: Arc<dyn MessageBroker>,
  db: Arc<dyn SqlClient>,
  (topic, group, options): (Topic, ConsumerGroup, SubscribeOptions),
) -> Effect<(), Never> {
  let run = notifications::consume(
    Arc::clone(&broker),
    topic.clone(),
    group.clone(),
    options.clone(),
    Arc::clone(&db),
  );
  run.exit::<Never>().flat_map(move |exit| {
    eprintln!("ledger: notifications consumer stopped ({exit:?}); subscribing again");
    sleep(RESUBSCRIBE).flat_map(move |()| subscribe(broker, db, (topic, group, options)))
  })
}
