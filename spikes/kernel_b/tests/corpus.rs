//! Spike B over the shared handler corpus (`spikes/corpus/handlers.md`).
//!
//! B1 style throughout (design §2.2): handlers are generic `fn h<R: Has<..>>() -> Effect<_, _, R>`,
//! envs are concrete `#[derive(Env)]` structs (or a single service, which is its own env) built at
//! the edge. `~Cap` reads a capability, `~ eff` binds an effect, both inside `effect!(|r| ..)`.

use std::time::{Duration, Instant};

use kernel_b::{Cause, Effect, Env, Exit, Has, Layer, Never, Replace, effect};
use spike_corpus::{
  AppError, Cache, ConfigError, ConfigSource, Counter, CurrentUser, Db, DbError, Greeting, Hits,
  Logger, Mailer, Req, Sessions, Tenant,
};

// corpus:begin H01
fn hello<R: Has<Greeting>>() -> Effect<String, Never, R> {
  effect!(|r| ~Greeting.0.to_string())
}

#[test]
fn h01_single_capability_read() {
  let out = hello()
    .provide(Greeting("hello from effect-axum"))
    .block_on();
  assert_eq!(out, Ok("hello from effect-axum".to_string()));
}
// corpus:end H01

// corpus:begin H02
fn answer() -> Effect<u32> {
  kernel_b::succeed(42)
}

#[derive(Clone, Env)]
struct GreetEnv {
  greeting: Greeting,
}

/// The env-free `answer()` embedded as a step of an `R = {Greeting}` effect. The `()`-lift
/// (`IntoBind<R> for Effect<_, _, ()>`) needs `R: Env`, so a bare `Greeting` env does not do.
fn greet_and_answer<R: Has<Greeting> + Env>() -> Effect<String, Never, R> {
  effect!(|r| {
    let n = ~ answer();
    format!("{}:{n}", ~Greeting.0)
  })
}

#[test]
fn h02_capability_free_value() {
  assert_eq!(answer().block_on(), Ok(42));
  let env = GreetEnv {
    greeting: Greeting("hi"),
  };
  assert_eq!(
    greet_and_answer().provide(env).block_on(),
    Ok("hi:42".to_string())
  );
}
// corpus:end H02

// corpus:begin H03
fn async_step() -> Effect<String> {
  kernel_b::sleep(Duration::from_millis(1)).map(|()| "async step finished".to_string())
}

#[test]
fn h03_real_async_step() {
  let started = Instant::now();
  assert_eq!(
    async_step().block_on(),
    Ok("async step finished".to_string())
  );
  assert!(started.elapsed() >= Duration::from_millis(1));
}
// corpus:end H03

// corpus:begin H04
fn count_str<R: Has<Counter>>() -> Effect<String, Never, R> {
  effect!(|r| {
    let c = ~Counter;
    format!("count={}", c.0)
  })
}

#[test]
fn h04_capability_bound_inside_effect_macro() {
  assert_eq!(
    count_str().provide(Counter(7)).block_on(),
    Ok("count=7".to_string())
  );
}
// corpus:end H04

// corpus:begin H05
fn execute<R, F>(env: R, f: F) -> String
where
  R: Send + 'static,
  F: FnOnce() -> Effect<String, Never, R>,
{
  match f().provide(env).block_on() {
    Ok(s) => s,
    Err(cause) => panic!("handler failed: {cause:?}"),
  }
}

#[test]
fn h05_inline_effect_handed_to_a_generic_runner() {
  let out = execute(Counter(7), || effect!(|r| format!("n={}", ~Counter.0)));
  assert_eq!(out, "n=7");
}
// corpus:end H05

// corpus:begin H06
fn session_user<R: Has<Sessions>>() -> Effect<String, Never, R> {
  effect!(|r| {
    let user = ~Sessions.get("demo").await;
    format!("user={}", user.as_deref().unwrap_or("none"))
  })
}

#[test]
fn h06_async_service_method_composed() {
  let sessions = Sessions::default();
  assert_eq!(
    session_user().provide(sessions.clone()).block_on(),
    Ok("user=none".to_string())
  );
  sessions.put("demo", "user-1");
  assert_eq!(
    session_user().provide(sessions).block_on(),
    Ok("user=user-1".to_string())
  );
}
// corpus:end H06

// corpus:begin H07
struct Service<F> {
  f: F,
}

impl<F: Fn(u32) -> Effect<u32>> Service<F> {
  fn new(f: F) -> Self {
    Self { f }
  }

  fn call(&self, x: u32) -> Exit<u32, Never> {
    (self.f)(x).block_on()
  }
}

#[test]
fn h07_handler_as_a_value() {
  let service = Service::new(|x| kernel_b::succeed(x.saturating_add(1)));
  assert_eq!(service.call(41), Ok(42));
}
// corpus:end H07

// corpus:begin H08
fn hit<R: Has<Hits>>(label: &'static str) -> Effect<String, Never, R> {
  effect!(|r| format!("{label} #{}", ~Hits.bump()))
}

#[test]
fn h08_shared_state_env_reused_across_runs() {
  let env = Hits::default();
  assert_eq!(
    hit("req").provide(env.clone()).block_on(),
    Ok("req #1".to_string())
  );
  assert_eq!(
    hit("req").provide(env.clone()).block_on(),
    Ok("req #2".to_string())
  );
  assert_eq!(env.get(), 2);
}
// corpus:end H08

// corpus:begin H09
fn tenant_name<R: Has<Tenant>>() -> Effect<String, Never, R> {
  effect!(|r| format!("tenant={}", ~Tenant.0))
}

fn serve_tenant<R: Replace<Tenant> + Clone>(base: &R, req: &Req) -> Exit<String, Never> {
  let handler = match req.get_header("x-tenant") {
    Some(t) => tenant_name().provide_service(Tenant(t.to_string())),
    None => tenant_name(),
  };
  handler.provide(base.clone()).block_on()
}

#[test]
fn h09_request_scoped_override() {
  let base = Tenant("public".into());
  let with = Req::new("/whoami").header("X-Tenant", "acme");
  assert_eq!(serve_tenant(&base, &with), Ok("tenant=acme".to_string()));
  assert_eq!(
    serve_tenant(&base, &Req::new("/whoami")),
    Ok("tenant=public".to_string())
  );
  assert_eq!(base, Tenant("public".into()));
}
// corpus:end H09

// corpus:begin H10
fn whoami<R: Has<Tenant> + Has<Logger> + Has<CurrentUser>>() -> Effect<String, AppError, R> {
  effect!(|r| {
    let user = ~CurrentUser;
    let tenant = ~Tenant;
    ~Logger.info(format!("whoami {}", user.id));
    format!("{}@{}", user.id, tenant.0)
  })
}

#[derive(Clone, Env)]
struct TenantAppEnv {
  tenant: Tenant,
  logger: Logger,
}

/// B §2.3: "add a service" is spelled "build a bigger struct" (one per app).
#[derive(Clone, Env)]
struct RequestEnv {
  #[env(flatten)]
  app: TenantAppEnv,
  user: CurrentUser,
}

fn serve_whoami(app: &TenantAppEnv, req: &Req) -> Exit<String, AppError> {
  let Some(id) = req.get_header("x-user") else {
    return Err(Cause::Fail(AppError::Unauthorized));
  };
  let env = RequestEnv {
    app: app.clone(),
    user: CurrentUser { id: id.to_string() },
  };
  whoami().provide(env).block_on()
}

#[test]
fn h10_request_scoped_addition() {
  let app = TenantAppEnv {
    tenant: Tenant("acme".into()),
    logger: Logger::default(),
  };
  let req = Req::new("/whoami").header("x-user", "u1");
  assert_eq!(serve_whoami(&app, &req), Ok("u1@acme".to_string()));
  assert_eq!(
    serve_whoami(&app, &Req::new("/whoami")),
    Err(Cause::Fail(AppError::Unauthorized))
  );
  assert_eq!(app.logger.lines(), vec!["whoami u1".to_string()]);
}
// corpus:end H10

// corpus:begin H11
fn count_users<R: Has<Db>>() -> Effect<String, DbError, R> {
  effect!(|r| ~Db.count("users").await?.to_string())
}

#[test]
fn h11_sql_count_with_typed_error() {
  assert_eq!(
    count_users().provide(Db::with_rows("users", 3)).block_on(),
    Ok("3".to_string())
  );
  let failing = Db::default();
  failing.set_failing(true);
  assert_eq!(
    count_users().provide(failing).block_on(),
    Err(Cause::Fail(DbError("unavailable".into())))
  );
}
// corpus:end H11

// corpus:begin H12
fn insert_rows<R: Has<Db> + Has<Logger>>(rows: Vec<&'static str>) -> Effect<(), DbError, R> {
  kernel_b::acquire_use_release(
    effect!(|r| ~Db.begin().await?),
    move |tx| {
      effect!(|r| {
        for row in &rows {
          tx.insert("items", row)?;
        }
        tx.commit().await?;
      })
    },
    |tx, exit| {
      let committed = exit.is_ok();
      effect!(|r| {
        tx.rollback();
        ~Logger.info(if committed { "tx committed" } else { "tx rolled back" });
      })
    },
  )
}

#[derive(Clone, Env)]
struct TxEnv {
  db: Db,
  logger: Logger,
}

#[test]
fn h12_transaction_with_rollback_finalizer() {
  let env = TxEnv {
    db: Db::default(),
    logger: Logger::default(),
  };
  assert_eq!(
    insert_rows(vec!["a", "b"]).provide(env.clone()).block_on(),
    Ok(())
  );
  assert_eq!(env.db.committed_rows("items"), 2);
  assert_eq!(
    insert_rows(vec!["a", "boom"])
      .provide(env.clone())
      .block_on(),
    Err(Cause::Fail(DbError("constraint violated in items".into())))
  );
  assert_eq!(env.db.committed_rows("items"), 2);
  assert_eq!(env.db.rollbacks(), 1);
  assert_eq!(env.logger.lines(), vec!["tx committed", "tx rolled back"]);
}
// corpus:end H12

// corpus:begin H13
fn app_name<R: Has<ConfigSource>>() -> Effect<String, ConfigError, R> {
  effect!(|r| ~ConfigSource.get_string("app_name")?)
}

#[test]
fn h13_config_read() {
  let cfg = ConfigSource::from_pairs([("app_name", "example")]);
  assert_eq!(
    app_name().provide(cfg).block_on(),
    Ok("example".to_string())
  );
  assert_eq!(
    app_name().provide(ConfigSource::default()).block_on(),
    Err(Cause::Fail(ConfigError::Missing("app_name".into())))
  );
}
// corpus:end H13

// corpus:begin H14
fn region<R: Has<ConfigSource>>() -> Effect<String, ConfigError, R> {
  effect!(|r| ~ConfigSource.get_string("REGION")?)
}

fn regions<R: Replace<ConfigSource>>() -> Effect<String, ConfigError, R> {
  effect!(|r| {
    let outer = ~ region();
    let inner = ~ region().provide_service(ConfigSource::from_pairs([("REGION", "eu-west")]));
    let again = ~ region();
    format!("{outer}/{inner}/{again}")
  })
}

#[test]
fn h14_nested_override_within_one_effect() {
  let cfg = ConfigSource::from_pairs([("REGION", "us-east")]);
  assert_eq!(
    regions().provide(cfg).block_on(),
    Ok("us-east/eu-west/us-east".to_string())
  );
}
// corpus:end H14

// corpus:begin H15
fn count_table<R: Has<ConfigSource> + Has<Db> + Has<Logger>>() -> Effect<usize, AppError, R> {
  effect!(|r| {
    let table = ~ConfigSource.get_string("table").map_err(AppError::from)?;
    let n = ~Db.count(&table).await.map_err(AppError::from)?;
    ~Logger.info(format!("counted {n}"));
    n
  })
}

#[derive(Clone, Env)]
struct CountEnv {
  config: ConfigSource,
  db: Db,
  logger: Logger,
}

#[test]
fn h15_three_capabilities_and_an_error_union() {
  let env = CountEnv {
    config: ConfigSource::from_pairs([("table", "users")]),
    db: Db::with_rows("users", 3),
    logger: Logger::default(),
  };
  assert_eq!(count_table().provide(env.clone()).block_on(), Ok(3));
  assert_eq!(env.logger.lines(), vec!["counted 3"]);
  let missing = CountEnv {
    config: ConfigSource::default(),
    ..env.clone()
  };
  assert_eq!(
    count_table().provide(missing).block_on(),
    Err(Cause::Fail(AppError::Config(ConfigError::Missing(
      "table".into()
    ))))
  );
  env.db.set_failing(true);
  assert_eq!(
    count_table().provide(env).block_on(),
    Err(Cause::Fail(AppError::Db(DbError("unavailable".into()))))
  );
}
// corpus:end H15

// corpus:begin H16
fn log_line<R: Has<Logger>>(msg: &'static str) -> Effect<(), Never, R> {
  effect!(|r| ~Logger.info(msg))
}

fn greet_count<R: Has<Db> + Has<Logger> + Has<Greeting>>() -> Effect<String, DbError, R> {
  effect!(|r| {
    ~ log_line("greet_count");
    let n = ~ count_users();
    let greeting = ~ hello();
    format!("{greeting}:{n}")
  })
}

#[derive(Clone, Env)]
struct GreetCountEnv {
  greeting: Greeting,
  logger: Logger,
  db: Db,
}

#[test]
fn h16_composition_with_subsumption() {
  let env = GreetCountEnv {
    greeting: Greeting("hi"),
    logger: Logger::default(),
    db: Db::with_rows("users", 3),
  };
  assert_eq!(
    greet_count().provide(env.clone()).block_on(),
    Ok("hi:3".to_string())
  );
  assert_eq!(env.logger.lines(), vec!["greet_count"]);
}
// corpus:end H16

// corpus:begin H17
fn audit<R: Has<Logger>>(msg: String) -> Effect<(), Never, R> {
  effect!(|r| ~Logger.info(msg))
}

fn audited_count<R: Has<Logger> + Has<Db>>() -> Effect<usize, AppError, R> {
  effect!(|r| {
    let n = ~Db.count("users").await.map_err(AppError::from)?;
    ~ audit(format!("counted {n}"));
    n
  })
}

fn audited_mail<R: Has<Logger> + Has<Mailer>>() -> Effect<(), AppError, R> {
  effect!(|r| {
    ~Mailer.send("ops@example.com", "hi").await.map_err(AppError::from)?;
    ~ audit("mailed".to_string());
  })
}

#[derive(Clone, Env)]
struct AuditDbEnv {
  logger: Logger,
  db: Db,
}

#[derive(Clone, Env)]
struct AuditMailEnv {
  logger: Logger,
  mailer: Mailer,
}

#[test]
fn h17_one_generic_helper_two_envs() {
  let logger = Logger::default();
  let db_env = AuditDbEnv {
    logger: logger.clone(),
    db: Db::with_rows("users", 3),
  };
  let mail_env = AuditMailEnv {
    logger: logger.clone(),
    mailer: Mailer::default(),
  };
  assert_eq!(audited_count().provide(db_env).block_on(), Ok(3));
  assert_eq!(audited_mail().provide(mail_env).block_on(), Ok(()));
  assert_eq!(logger.lines(), vec!["counted 3", "mailed"]);
}
// corpus:end H17

// corpus:begin H18
fn users_or_cached<R: Has<Db> + Has<Cache>>() -> Effect<String, Never, R> {
  count_users().catch(|_db_error| {
    effect!(|r| {
      let cached = ~Cache.get("users_count").unwrap_or_else(|| "0".to_string());
      format!("cached:{cached}")
    })
  })
}

#[derive(Clone, Env)]
struct CachedEnv {
  db: Db,
  cache: Cache,
}

#[test]
fn h18_recover_with_another_capability() {
  let env = CachedEnv {
    db: Db::with_rows("users", 3),
    cache: Cache::default(),
  };
  assert_eq!(
    users_or_cached().provide(env.clone()).block_on(),
    Ok("3".to_string())
  );
  env.db.set_failing(true);
  env.cache.put("users_count", "7");
  assert_eq!(
    users_or_cached().provide(env).block_on(),
    Ok("cached:7".to_string())
  );
}
// corpus:end H18

// corpus:begin H19
fn report<R: Has<Db> + Has<Mailer> + Clone>() -> Effect<(usize, ()), AppError, R> {
  kernel_b::zip_par(
    effect!(|r| ~Db.count("users").await.map_err(AppError::from)?),
    effect!(|r| ~Mailer.send("ops@example.com", "report").await.map_err(AppError::from)?),
  )
}

#[derive(Clone, Env)]
struct ReportEnv {
  db: Db,
  mailer: Mailer,
}

#[test]
fn h19_parallel_fan_out() {
  let env = ReportEnv {
    db: Db::with_rows("users", 3),
    mailer: Mailer::default(),
  };
  assert_eq!(report().provide(env.clone()).block_on(), Ok((3, ())));
  assert_eq!(env.mailer.sent().len(), 1);
  env.db.set_failing(true);
  assert_eq!(
    report().provide(env).block_on(),
    Err(Cause::Fail(AppError::Db(DbError("unavailable".into()))))
  );
}
// corpus:end H19

// corpus:begin H20
fn bounded_count<R: Has<Db> + Clone>(hang: bool) -> Effect<usize, AppError, R> {
  effect!(|r| {
    let db = ~Db;
    let n = if hang { db.hang().await } else { db.count("users").await };
    n.map_err(AppError::from)?
  })
  .timeout_fail(Duration::from_millis(50), || AppError::Timeout)
}

#[test]
fn h20_timeout_around_a_hanging_call() {
  let db = Db::with_rows("users", 3);
  assert_eq!(bounded_count(false).provide(db.clone()).block_on(), Ok(3));
  let started = Instant::now();
  assert_eq!(
    bounded_count(true).provide(db).block_on(),
    Err(Cause::Fail(AppError::Timeout))
  );
  assert!(started.elapsed() < Duration::from_millis(500));
}
// corpus:end H20

// corpus:begin W01
#[derive(Clone, Env)]
struct BaseEnv {
  config: ConfigSource,
  logger: Logger,
}

#[derive(Clone, Env)]
struct InfraEnv {
  #[env(flatten)]
  base: BaseEnv,
  db: Db,
  sessions: Sessions,
  cache: Cache,
}

#[derive(Clone, Env)]
struct AppEnv {
  #[env(flatten)]
  infra: InfraEnv,
  greeting: Greeting,
  counter: Counter,
  hits: Hits,
  tenant: Tenant,
  mailer: Mailer,
}

fn base_layer(config: ConfigSource) -> Layer<BaseEnv, AppError> {
  Layer::from_effect(kernel_b::succeed(BaseEnv {
    config,
    logger: Logger::default(),
  }))
}

/// `Layer::merge` yields a tuple; the union is named by hand with the `InfraEnv` struct.
fn infra_layer() -> Layer<InfraEnv, AppError, BaseEnv> {
  let services = Layer::from_effect(effect!(|r| {
    let url = ~ConfigSource.get_string("db_url").map_err(AppError::from)?;
    ~Logger.info(format!("db connect {url}"));
    (Db::with_rows("users", 3), Sessions::default(), Cache::default())
  }));
  Layer::identity()
    .merge(services)
    .map(|(base, (db, sessions, cache))| InfraEnv {
      base,
      db,
      sessions,
      cache,
    })
}

fn app_layer() -> Layer<AppEnv, AppError, InfraEnv> {
  Layer::identity().map(|infra| AppEnv {
    infra,
    greeting: Greeting("hello from effect-axum"),
    counter: Counter(7),
    hits: Hits::default(),
    tenant: Tenant("public".into()),
    mailer: Mailer::default(),
  })
}

#[test]
fn w01_edge_wiring_of_the_showcase_env() {
  let config = ConfigSource::from_pairs([("db_url", "mem://app"), ("table", "users")]);
  let env = base_layer(config)
    .and_then(infra_layer())
    .and_then(app_layer())
    .build()
    .block_on()
    .expect("app env builds");
  assert_eq!(
    hello().provide(env.clone()).block_on(),
    Ok("hello from effect-axum".to_string())
  );
  assert_eq!(
    count_users().provide(env.clone()).block_on(),
    Ok("3".to_string())
  );
  assert_eq!(count_table().provide(env.clone()).block_on(), Ok(3));
  assert_eq!(
    greet_count().provide(env.clone()).block_on(),
    Ok("hello from effect-axum:3".to_string())
  );
  assert_eq!(audited_count().provide(env.clone()).block_on(), Ok(3));
  assert_eq!(audited_mail().provide(env.clone()).block_on(), Ok(()));
  assert_eq!(
    env.infra.base.logger.lines(),
    vec![
      "db connect mem://app",
      "counted 3",
      "greet_count",
      "counted 3",
      "mailed"
    ]
  );
}
// corpus:end W01
