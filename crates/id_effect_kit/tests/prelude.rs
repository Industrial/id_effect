//! The prelude and the crate re-exports resolve to working items, per feature.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use id_effect_kit::id_effect::{HCons, HNil};
use id_effect_kit::prelude::*;

#[derive(Debug, PartialEq)]
struct Boom;

#[derive(Debug, PartialEq)]
struct Other;

struct Greeting(&'static str);

#[test]
fn succeed_and_fail_run_to_their_exit() {
  assert!(matches!(
    succeed::<_, Boom, _>(7).block_on(),
    Exit::Success(7)
  ));
  assert!(matches!(
    fail::<u8, _, _>(Boom).block_on(),
    Exit::Failure(Cause::Fail(Boom))
  ));
}

#[test]
fn err_macro_spells_an_or_channel() {
  let eff: Effect<u8, err!(Boom | Other)> = fail(Or::Right(Other));
  assert!(matches!(
    eff.block_on(),
    Exit::Failure(Cause::Fail(Or::Right(Other)))
  ));
}

#[test]
fn need_reads_a_capability_provided_as_an_hlist() {
  let eff: Effect<String, Boom, HCons<Greeting, HNil>> =
    need::<Greeting>().map(|g| format!("{} kit", g.0));
  let out = eff.provide_env(hlist![Greeting("hello")]).block_on();
  assert!(matches!(out, Exit::Success(s) if s == "hello kit"));
}

#[test]
fn retry_follows_a_schedule() {
  let calls = Arc::new(AtomicU32::new(0));
  let counter = Arc::clone(&calls);
  let eff: Effect<u32, Boom> = Effect::retry(
    move || {
      let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
      if n < 3 { fail(Boom) } else { succeed(n) }
    },
    Schedule::recurs(5),
  );
  assert!(matches!(eff.block_on(), Exit::Success(3)));
  assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn from_async_and_sleep_compose() {
  let eff: Effect<u8, Boom> =
    sleep(std::time::Duration::from_millis(1)).flat_map(|()| from_async(|| async { Ok(5) }));
  assert!(matches!(eff.block_on(), Exit::Success(5)));
}

#[test]
fn secret_never_prints_its_value() {
  let token = Secret::new("hunter2".to_string());
  assert_eq!(format!("{token:?}"), "<redacted>");
  assert_eq!(token.expose(), "hunter2");
}

#[test]
fn run_main_maps_success_and_failure_to_exit_codes() {
  let ok = run_main(
    succeed::<_, Boom, _>(()),
    hlist![],
    RunMainConfig::minimal().ctrl_c(false),
  );
  assert_eq!(ok, ExitCode::SUCCESS);
  let failed = run_main(
    fail::<(), _, _>(Boom),
    hlist![],
    RunMainConfig::minimal().ctrl_c(false),
  );
  assert_eq!(failed, ExitCode::from(1));
}

#[test]
fn always_on_crates_are_re_exported_whole() {
  let eff: Effect<u8, Boom> = id_effect_kit::id_effect::succeed(1);
  assert!(matches!(eff.block_on(), Exit::Success(1)));
  let config = id_effect_kit::cli::RunMainConfig::with_tracing();
  assert!(config.install_tracing);
  let secret = id_effect_kit::config::Secret::new(3_u8);
  assert_eq!(secret.into_inner(), 3);
}

#[cfg(feature = "web")]
#[tokio::test]
async fn web_prelude_routes_answer() {
  use axum::body::Body;
  use axum::http::{Request, StatusCode};
  use tower::ServiceExt;

  for (router, path, status) in [
    (health_router(), "/health", StatusCode::OK),
    (
      observability_routes(Arc::new(|| true)).merge(metrics_router()),
      "/metrics",
      StatusCode::OK,
    ),
    (
      readiness_router(Arc::new(|| true)),
      "/ready",
      StatusCode::OK,
    ),
    (
      observability_routes(Arc::new(|| false)),
      "/ready",
      StatusCode::SERVICE_UNAVAILABLE,
    ),
  ] {
    let req = Request::builder().uri(path).body(Body::empty()).unwrap();
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), status, "{path}");
  }
}

#[cfg(feature = "web")]
#[test]
fn web_crate_and_executor_are_reachable() {
  let _executor: EffectExecutor = id_effect_kit::axum::EffectExecutor::new();
  assert!(std::any::type_name::<SecurityLayer>().ends_with("SecurityLayer"));
}

#[cfg(feature = "sql-pg")]
#[test]
fn with_transaction_commits_on_success() {
  use id_effect_kit::sql::{SqlClient, TestSqlClient};

  let test = TestSqlClient::new();
  assert!(matches!(test.connect().block_on(), Exit::Success(())));
  let service = SqlClientService::new(test.clone());
  let eff: Effect<u8, SqlError> = with_transaction(Arc::clone(&service.0), |_tx| succeed(9));
  assert!(matches!(eff.block_on(), Exit::Success(9)));
  assert_eq!(test.commits(), 1);
}

#[cfg(feature = "otel")]
#[test]
fn otel_config_defaults_to_a_local_collector() {
  let config = OtelConfig::localhost("kit");
  assert_eq!(config.service_name, "kit");
  let install: fn(OtelConfig) -> _ = install_from_config;
  let _ = install;
  assert!(
    id_effect_kit::otel::OtelConfig::localhost("x")
      .endpoint
      .contains("4317")
  );
}

#[cfg(feature = "cluster")]
#[test]
fn cluster_crate_is_reachable() {
  assert!(std::any::type_name::<id_effect_kit::node::NodeId>().ends_with("NodeId"));
}

#[test]
fn profiles_resolve_a_known_profile() {
  let profiles = Profiles::new().known(["dev", "prod"]);
  assert_eq!(profiles.profile("prod").unwrap().as_str(), "prod");
  assert!(profiles.profile("staging").is_err());
}

/// A typed route (`GET /twice/{n}`, `n` in 1..=10) shared by the web and openapi tests.
#[cfg(feature = "web")]
mod typed {
  use id_effect_kit::id_effect::schema::{self, HasSchema};
  use id_effect_kit::prelude::*;

  pub struct Small;

  impl HasSchema for Small {
    type A = i64;
    type I = i64;
    type E = ();

    fn schema() -> Schema<i64, i64> {
      schema::range(schema::i64::<()>(), 1, 10)
    }
  }

  fn twice(_: &mut (), input: Input<i64>) -> Effect<String, std::convert::Infallible, ()> {
    succeed((input.path * 2).to_string())
  }

  pub fn router() -> Router {
    Router::new().route(Route::get("/twice/{n}").path::<Small>().handler(twice))
  }
}

#[cfg(feature = "web")]
#[tokio::test]
async fn typed_routes_answer_problems_through_the_middleware_stack() {
  use axum::body::Body;
  use axum::http::{Request, StatusCode, header};
  use tower::ServiceExt;

  let app: axum::Router = typed::router()
    .with_state::<()>(())
    .into_axum()
    .layer(CompressionLayer::new())
    .layer(TimeoutLayer::default())
    .layer(BodyLimitLayer::default())
    .layer(RequestIdLayer::new());
  let get = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();

  let ok = app.clone().oneshot(get("/twice/4")).await.unwrap();
  assert_eq!(ok.status(), StatusCode::OK);
  assert!(ok.headers().contains_key("x-request-id"));

  let bad = app.oneshot(get("/twice/40")).await.unwrap();
  assert_eq!(bad.status(), StatusCode::UNPROCESSABLE_ENTITY);
  assert_eq!(
    bad.headers()[header::CONTENT_TYPE],
    id_effect_kit::axum::PROBLEM_JSON
  );
  assert!(std::any::type_name::<Problem>().ends_with("Problem"));
  assert!(std::any::type_name::<ValidJson<typed::Small>>().contains("ValidJson"));
}

#[cfg(feature = "openapi")]
#[test]
fn openapi_documents_the_typed_routes() {
  let json = OpenApi::from_router(&typed::router()).to_json();
  assert_eq!(json["openapi"], "3.1.0");
  assert!(json["paths"]["/twice/{n}"]["get"]["responses"]["422"].is_object());
}

#[cfg(feature = "sql")]
#[test]
fn from_row_derives_through_the_kit_path() {
  /// The derive names `::id_effect_sql`; through the kit, point it at `id_effect_kit::sql`.
  #[derive(Debug, PartialEq, FromRow)]
  #[from_row(crate = id_effect_kit::sql)]
  struct Balance {
    id: i64,
    cents: i64,
  }

  use id_effect_kit::sql::FromRow as _;
  let row = SqlRow::from_pairs([("id", 1_i64), ("cents", 250)]);
  assert_eq!(
    Balance::from_row(&row).unwrap(),
    Balance { id: 1, cents: 250 }
  );
  assert_eq!(params![1_i64, "x"].len(), 2);
  assert!(std::any::type_name::<SqlError>().ends_with("SqlError"));
}

#[cfg(feature = "migrate")]
#[test]
fn migrator_reads_sqlx_style_files() {
  let migrator: Result<Migrator, MigrateError> =
    Migrator::from_files([("0001_init.sql", "CREATE TABLE t (id BIGINT)")]);
  let migrator = migrator.unwrap();
  assert_eq!(migrator.migrations().len(), 1);
  assert_eq!(migrator.migrations()[0].version(), 1);
}

#[cfg(feature = "repository")]
#[test]
fn entity_derives_through_the_kit_path() {
  use id_effect_kit::sql::Dialect;

  #[derive(Debug, FromRow, Entity)]
  #[from_row(crate = id_effect_kit::sql)]
  #[entity(crate = id_effect_kit::sql, table = "accounts")]
  struct Account {
    #[entity(id)]
    id: i64,
    #[entity(version)]
    version: i64,
  }

  use id_effect_kit::sql::Entity as _;
  assert_eq!(Account::TABLE, "accounts");
  assert_eq!(Account::VERSION, Some("version"));
  let repo = Repository::<Account>::new(Dialect::Postgres);
  assert_eq!(repo.dialect(), Dialect::Postgres);
}

#[cfg(feature = "auth")]
#[test]
fn jwts_round_trip_and_feed_authenticate() {
  let secret = b"a 256-bit secret for HS256 tokens";
  let issuer = JwtIssuer::hs256(secret);
  let verifier = JwtVerifier::new([JwtKey::hmac(secret)]);
  let alice = AuthPrincipal::new("alice").with_role("accountant");
  let Exit::Success(token) = issuer.issue(&alice).block_on() else {
    panic!("issue failed")
  };
  let verified = verifier.verify(&token).block_on();
  assert!(matches!(verified, Exit::Success(ref p) if format!("{p:?}").contains("alice")));
  let _middleware = Authenticate::bearer_jwt(verifier);
  assert!(std::any::type_name::<Sessions>().ends_with("Sessions"));
  assert!(std::any::type_name::<AuthError>().ends_with("AuthError"));
}

#[cfg(feature = "authz")]
#[test]
fn authorize_checks_a_named_policy() {
  use id_effect_kit::authz::BasicPrincipal;

  let policies = Policies::builder()
    .define::<()>("admin-only", require_role("admin"))
    .build();
  let as_role = |role: &str| {
    let principal = CurrentPrincipal::new(BasicPrincipal::new("p").with_role(role));
    authorize::<(), _, _, _>("admin-only", ())
      .provide_all(hlist![principal, policies.clone()])
      .block_on()
  };
  assert!(matches!(as_role("admin"), Exit::Success(())));
  assert!(matches!(
    as_role("guest"),
    Exit::Failure(Cause::Fail(AuthzError::Denied(_)))
  ));
}

#[cfg(feature = "redis")]
#[test]
fn redis_config_is_built_without_connecting() {
  let config = RedisConfig::new("redis://127.0.0.1:6379");
  assert_eq!(config.url(), "redis://127.0.0.1:6379");
  assert!(std::any::type_name::<RedisClient>().ends_with("RedisClient"));
}

#[cfg(feature = "jobs")]
#[test]
fn a_worker_drains_a_memory_queue_through_the_kit() {
  use id_effect_kit::id_effect::{Executor, TestExecutor, UtcDateTime, WallClock, sync};
  use id_effect_kit::jobs::{JobQueueService, MemoryJobQueue};

  #[derive(Clone, Schema)]
  struct Welcome {
    id: i64,
  }

  impl Job for Welcome {
    const NAME: &'static str = "kit.welcome";
  }

  let ran = Arc::new(AtomicU32::new(0));
  let counter = Arc::clone(&ran);
  let registry = JobRegistry::new()
    .register::<Welcome, JobFailure>(move |job, _ctx: JobContext| {
      let counter = Arc::clone(&counter);
      sync(move || {
        assert_eq!(job.id, 7);
        counter.fetch_add(1, Ordering::SeqCst);
      })
    })
    .unwrap();
  let exec = TestExecutor::new();
  let clock = WallClock::anchored(exec.now(), UtcDateTime::unsafe_make(1_800_000_000_000));
  let store = MemoryJobQueue::new(clock);
  let env = hlist![JobQueueService::new(store.clone())];
  let program = store
    .enqueue(Welcome { id: 7 }, JobOptions::default())
    .flat_map(move |_| Worker::new(registry).run_until_idle().provide_all(env));
  let done = program.fork_root(Arc::new(exec.clone()));
  exec.run_until_stalled();
  assert!(done.is_done());
  assert_eq!(ran.load(Ordering::SeqCst), 1);
  assert!(WorkerConfig::default().poll_interval > std::time::Duration::ZERO);
}

#[cfg(feature = "jobs-cron")]
#[test]
fn cron_leader_is_in_the_prelude() {
  assert!(std::any::type_name::<CronLeader>().ends_with("CronLeader"));
}

#[cfg(feature = "messaging")]
#[test]
fn memory_broker_keeps_published_messages() {
  use id_effect_kit::messaging::{MemoryBroker, Message, Topic};

  let broker = MemoryBroker::new();
  let topic = Topic::new("ledger.posted").unwrap();
  let published = broker.publish(&topic, Message::new("hi")).block_on();
  assert!(matches!(published, Exit::Success(())));
  assert_eq!(broker.messages(&topic).len(), 1);
  assert!(std::any::type_name::<BrokerError>().ends_with("BrokerError"));
}

#[cfg(feature = "redis-streams")]
#[test]
fn redis_streams_broker_is_configured_without_connecting() {
  let config = id_effect_kit::redis::RedisStreamsConfig::new().max_len(10);
  assert_eq!(config.max_len_cap(), Some(10));
  assert!(std::any::type_name::<RedisStreamsBroker>().ends_with("RedisStreamsBroker"));
}

#[cfg(feature = "mail")]
#[test]
fn mailboxes_hold_a_validated_address() {
  use id_effect_kit::mail::Address;

  let to = Mailbox::named("Ada", Address::parse("ada@ledger.example").unwrap());
  assert_eq!(to.name(), Some("Ada"));
  assert_eq!(to.address().domain(), "ledger.example");
  assert!(Address::parse("ada@").is_err());
  assert!(std::any::type_name::<MailError>().ends_with("MailError"));
}

#[cfg(all(feature = "mail", feature = "jobs"))]
#[test]
fn mail_delivery_job_is_reachable() {
  assert!(std::any::type_name::<id_effect_kit::mail::DeliverMail>().ends_with("DeliverMail"));
}

#[cfg(feature = "auth-accounts")]
#[test]
fn accounts_and_their_mails_are_reachable() {
  use id_effect_kit::auth::{AccountNotifier, Accounts, PgAccountTokenStore};
  use id_effect_kit::mail::AccountMails;

  fn notifier<N: AccountNotifier>() {}
  notifier::<AccountMails>();
  assert!(std::any::type_name::<Accounts>().ends_with("Accounts"));
  assert!(std::any::type_name::<PgAccountTokenStore>().ends_with("PgAccountTokenStore"));
}

#[cfg(feature = "console")]
#[test]
fn console_routes_come_behind_a_guard() {
  use id_effect_kit::authz::Guard;

  assert!(!ConsoleConfig::loopback(9090).requires_guard());
  let routes: axum::Router = Console::new().router(Guard::new("console", require_role("ops")));
  let _ = routes;
}

#[cfg(feature = "storage")]
#[test]
fn memory_store_is_an_object_store() {
  fn store(_: &impl ObjectStore) {}

  let memory = MemoryStore::new();
  store(&memory);
  assert!(memory.part_size() > 0);
  assert!(std::any::type_name::<StorageError>().ends_with("StorageError"));
}

#[cfg(feature = "events")]
#[test]
fn event_store_types_are_reachable() {
  assert!(std::any::type_name::<EventStoreError>().ends_with("EventStoreError"));
  let _ = id_effect_kit::events::SQL_EVENT_JOURNAL_DDL;
}

#[cfg(feature = "test")]
#[test]
fn test_crate_is_reachable() {
  assert!(!id_effect_kit::test::REQUIRE_DOCKER_ENV.is_empty());
  let client = id_effect_kit::test::TestClient::new(axum::Router::<()>::new());
  let _ = client.service();
}
