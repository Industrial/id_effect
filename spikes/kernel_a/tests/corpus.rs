//! The spike handler corpus (`spikes/corpus/handlers.md`) in Design A.
//!
//! Style: `effect!` with `~Cap` reads and `~effect` binds (the design's recommended surface,
//! §3), combinators where they are the natural spelling. Every shape is implemented; none is
//! declared unsupported. Two judgement calls are flagged inline (`// note:`): H05's runner ties
//! `R` to the env it is given (as 0.6 `execute` does), and H17's callers name their `R` because
//! a generic-`R` helper cannot be `~`-bound without it.

use std::time::Duration;

use kernel_a::{
  Cause, Effect, HList, Layer, Never, Selector, acquire_use_release, caps, effect, hlist, sleep,
  succeed,
};
use spike_corpus::{
  AppError, Cache, ConfigError, ConfigSource, Counter, CurrentUser, Db, DbError, Greeting, Hits,
  Logger, Mailer, Req, Sessions, Tenant,
};

// corpus:begin H01
fn hello() -> Effect<String, Never, caps!(Greeting)> {
  effect!({ ~Greeting.0.to_string() })
}

#[test]
fn h01_single_capability_read() {
  let out = hello()
    .provide(Greeting("hello from effect-axum"))
    .block_on();
  assert_eq!(out.unwrap(), "hello from effect-axum");
}
// corpus:end H01

// corpus:begin H02
fn answer() -> Effect<u32> {
  succeed(42)
}

fn greet_and_answer() -> Effect<String, Never, caps!(Greeting)> {
  effect!({
    let n = ~answer();
    format!("{}:{n}", ~Greeting.0)
  })
}

#[test]
fn h02_capability_free_value() {
  assert_eq!(answer().block_on().unwrap(), 42);
  let out = greet_and_answer().provide(Greeting("hi")).block_on();
  assert_eq!(out.unwrap(), "hi:42");
}
// corpus:end H02

// corpus:begin H03
fn async_step() -> Effect<String> {
  sleep(Duration::from_millis(1)).map(|()| "async step finished".to_string())
}

#[test]
fn h03_real_async_step() {
  assert_eq!(async_step().block_on().unwrap(), "async step finished");
}
// corpus:end H03

// corpus:begin H04
fn count_label() -> Effect<String, Never, caps!(Counter)> {
  effect!({
    let c = ~Counter;
    format!("count={}", c.0)
  })
}

#[test]
fn h04_capability_bound_inside_effect_macro() {
  assert_eq!(
    count_label().provide(Counter(7)).block_on().unwrap(),
    "count=7"
  );
}
// corpus:end H04

// corpus:begin H05
// note: the runner ties `R` to the env value it is handed, like 0.6 `execute(State<S>, F)` where
// `F: FnOnce(&mut S) -> Effect<_, _, S>`. A runner generic over a *subset* `R` cannot infer `R`
// from an unannotated closure (E0282): see tests/ui/generic_runner_unannotated.rs.
fn execute<R, F>(env: R, f: F) -> String
where
  R: HList,
  F: FnOnce() -> Effect<String, Never, R>,
{
  match f().provide_all(env).block_on() {
    Ok(s) => s,
    Err(c) => format!("error: {c:?}"),
  }
}

#[test]
fn h05_inline_effect_handed_to_a_generic_runner() {
  let out = execute(hlist![Counter(7)], || {
    effect!({ format!("n={}", ~Counter.0) })
  });
  assert_eq!(out, "n=7");
}
// corpus:end H05

// corpus:begin H06
fn session_user() -> Effect<String, Never, caps!(Sessions)> {
  effect!({
    match ~Sessions.get("demo").await {
      Some(user) => format!("user={user}"),
      None => "user=none".to_string(),
    }
  })
}

#[test]
fn h06_async_capability_method_composed() {
  let seeded = Sessions::default();
  seeded.put("demo", "user-1");
  assert_eq!(
    session_user().provide(seeded).block_on().unwrap(),
    "user=user-1"
  );
  let empty = Sessions::default();
  assert_eq!(
    session_user().provide(empty).block_on().unwrap(),
    "user=none"
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

  fn call(&self, x: u32) -> Effect<u32> {
    (self.f)(x)
  }
}

#[test]
fn h07_request_input_handler_as_a_value() {
  let svc = Service::new(|x| succeed(x.saturating_add(1)));
  assert_eq!(svc.call(41).block_on().unwrap(), 42);
}
// corpus:end H07

// corpus:begin H08
fn bump(label: &'static str) -> Effect<String, Never, caps!(Hits)> {
  effect!({ format!("{label} #{}", ~Hits.bump()) })
}

#[test]
fn h08_shared_state_env_reused_across_runs() {
  let hits = Hits::default();
  let env = hlist![hits.clone()];
  assert_eq!(
    bump("req").provide_env(env.clone()).block_on().unwrap(),
    "req #1"
  );
  assert_eq!(
    bump("req").provide_env(env.clone()).block_on().unwrap(),
    "req #2"
  );
  assert_eq!(hits.get(), 2);
}
// corpus:end H08

// corpus:begin H09
fn tenant_name() -> Effect<String, Never, caps!(Tenant)> {
  effect!({ format!("tenant={}", ~Tenant.0) })
}

fn serve_tenant(base: &caps!(Tenant), req: &Req) -> String {
  let run = match req.get_header("x-tenant") {
    Some(t) => tenant_name().provide(Tenant(t.to_string())),
    None => tenant_name().provide_env(base.clone()),
  };
  run.block_on().unwrap()
}

#[test]
fn h09_request_scoped_override() {
  let base = hlist![Tenant("public".to_string())];
  assert_eq!(
    serve_tenant(&base, &Req::new("/").header("x-tenant", "acme")),
    "tenant=acme"
  );
  assert_eq!(serve_tenant(&base, &Req::new("/")), "tenant=public");
  assert_eq!(base.0.0, "public");
}
// corpus:end H09

// corpus:begin H10
type WhoamiCaps = caps!(Tenant, Logger, CurrentUser);

fn whoami() -> Effect<String, AppError, WhoamiCaps> {
  effect!({
    let user = ~CurrentUser;
    let tenant = ~Tenant;
    ~Logger.info(format!("whoami {}", user.id));
    format!("{}@{}", user.id, tenant.0)
  })
}

fn serve_whoami(base: &caps!(Tenant, Logger), req: &Req) -> Result<String, Cause<AppError>> {
  let Some(id) = req.get_header("x-user") else {
    return Err(Cause::Fail(AppError::Unauthorized));
  };
  whoami()
    .provide(CurrentUser { id: id.to_string() })
    .provide_env(base.clone())
    .block_on()
}

#[test]
fn h10_request_scoped_addition() {
  let logger = Logger::default();
  let base = hlist![Tenant("acme".to_string()), logger.clone()];
  assert_eq!(
    serve_whoami(&base, &Req::new("/")),
    Err(Cause::Fail(AppError::Unauthorized))
  );
  assert!(logger.lines().is_empty());
  let req = Req::new("/").header("x-user", "u-1");
  assert_eq!(serve_whoami(&base, &req).unwrap(), "u-1@acme");
  assert_eq!(logger.lines(), vec!["whoami u-1".to_string()]);
}
// corpus:end H10

// corpus:begin H11
fn count_users() -> Effect<usize, DbError, caps!(Db)> {
  effect!({ ~Db.count("users").await? })
}

fn user_count() -> Effect<String, DbError, caps!(Db)> {
  count_users().map(|n| n.to_string())
}

#[test]
fn h11_sql_count_with_typed_error() {
  assert_eq!(
    user_count()
      .provide(Db::with_rows("users", 3))
      .block_on()
      .unwrap(),
    "3"
  );
  let db = Db::default();
  db.set_failing(true);
  assert_eq!(
    user_count().provide(db).block_on(),
    Err(Cause::Fail(DbError("unavailable".into())))
  );
}
// corpus:end H11

// corpus:begin H12
fn insert_all(rows: Vec<&'static str>) -> Effect<usize, DbError, caps!(Db, Logger)> {
  acquire_use_release(
    effect!({ ~Db.begin().await? }),
    move |tx| {
      effect!({
        for row in &rows {
          tx.insert("users", row)?;
        }
        tx.commit().await?;
        rows.len()
      })
    },
    |tx, exit| {
      effect!({
        tx.rollback();
        ~Logger.info(if exit.is_success() { "tx committed" } else { "tx rolled back" });
      })
    },
  )
}

#[test]
fn h12_transaction_with_rollback_finalizer() {
  let db = Db::default();
  let logger = Logger::default();
  let env = hlist![db.clone(), logger.clone()];
  assert_eq!(
    insert_all(vec!["a", "b"])
      .provide_env(env.clone())
      .block_on()
      .unwrap(),
    2
  );
  assert_eq!(db.committed_rows("users"), 2);
  let failed = insert_all(vec!["a", "boom"]).provide_env(env).block_on();
  assert_eq!(
    failed,
    Err(Cause::Fail(DbError("constraint violated in users".into())))
  );
  assert_eq!(db.committed_rows("users"), 2);
  assert_eq!(db.rollbacks(), 1);
  assert_eq!(logger.lines(), vec!["tx committed", "tx rolled back"]);
}
// corpus:end H12

// corpus:begin H13
fn app_name() -> Effect<String, ConfigError, caps!(ConfigSource)> {
  effect!({ ~ConfigSource.get_string("app_name")? })
}

#[test]
fn h13_config_read() {
  let cfg = ConfigSource::from_pairs([("app_name", "example")]);
  assert_eq!(app_name().provide(cfg).block_on().unwrap(), "example");
  assert_eq!(
    app_name().provide(ConfigSource::default()).block_on(),
    Err(Cause::Fail(ConfigError::Missing("app_name".into())))
  );
}
// corpus:end H13

// corpus:begin H14
fn region() -> Effect<String, ConfigError, caps!(ConfigSource)> {
  effect!({ ~ConfigSource.get_string("REGION")? })
}

fn regions() -> Effect<String, ConfigError, caps!(ConfigSource)> {
  effect!({
    let outer = ~region();
    let inner = ~region().provide(ConfigSource::from_pairs([("REGION", "eu-west")]));
    let again = ~region();
    format!("{outer}/{inner}/{again}")
  })
}

#[test]
fn h14_nested_override_within_one_effect() {
  let cfg = ConfigSource::from_pairs([("REGION", "us-east")]);
  assert_eq!(
    regions().provide(cfg).block_on().unwrap(),
    "us-east/eu-west/us-east"
  );
}
// corpus:end H14

// corpus:begin H15
type CountedCaps = caps!(ConfigSource, Db, Logger);

fn counted() -> Effect<usize, AppError, CountedCaps> {
  effect!({
    let table = ~ConfigSource.get_string("table")?;
    let n = ~Db.count(&table).await?;
    ~Logger.info(format!("counted {n}"));
    n
  })
}

#[test]
fn h15_three_capabilities_and_an_error_union() {
  let cfg = ConfigSource::from_pairs([("table", "users")]);
  let logger = Logger::default();
  let ok = hlist![cfg.clone(), Db::with_rows("users", 3), logger.clone()];
  assert_eq!(counted().provide_env(ok).block_on().unwrap(), 3);
  assert_eq!(logger.lines(), vec!["counted 3".to_string()]);
  let missing = hlist![ConfigSource::default(), Db::default(), logger.clone()];
  assert_eq!(
    counted().provide_env(missing).block_on(),
    Err(Cause::Fail(AppError::Config(ConfigError::Missing(
      "table".into()
    ))))
  );
  let db = Db::default();
  db.set_failing(true);
  let failing = hlist![cfg, db, logger];
  assert_eq!(
    counted().provide_env(failing).block_on(),
    Err(Cause::Fail(AppError::Db(DbError("unavailable".into()))))
  );
}
// corpus:end H15

// corpus:begin H16
fn log_line(msg: String) -> Effect<(), Never, caps!(Logger)> {
  effect!({ ~Logger.info(msg) })
}

type SummaryCaps = caps!(Db, Logger, Greeting);

fn summary() -> Effect<String, DbError, SummaryCaps> {
  effect!({
    let greeting = ~hello();
    let n = ~count_users();
    ~log_line(format!("summary for {n} users"));
    format!("{greeting}:{n}")
  })
}

#[test]
fn h16_composition_with_subsumption() {
  let logger = Logger::default();
  let env = hlist![Db::with_rows("users", 3), logger.clone(), Greeting("hi")];
  assert_eq!(summary().provide_env(env).block_on().unwrap(), "hi:3");
  assert_eq!(logger.lines(), vec!["summary for 3 users".to_string()]);
}
// corpus:end H16

// corpus:begin H17
fn audit<R, I>(msg: String) -> Effect<(), AppError, R>
where
  R: HList + Selector<Logger, I>,
{
  effect!({ ~Logger.info(msg) })
}

type SignupCaps = caps!(Logger, Db);
type NotifyCaps = caps!(Logger, Mailer);

// note: `~audit(..)` alone is ambiguous (E0283, tests/ui/generic_helper_bind.rs): `~` narrows
// from *any* sub-environment, so a helper generic in `R` gives it nothing to infer from.
fn signup() -> Effect<usize, AppError, SignupCaps> {
  effect!({
    let n = ~count_users();
    ~audit::<SignupCaps, _>(format!("signup: {n} users"));
    n
  })
}

fn notify() -> Effect<(), AppError, NotifyCaps> {
  effect!({
    ~Mailer.send("ops@example.com", "hello").await?;
    ~audit::<NotifyCaps, _>("notify: sent".to_string());
  })
}

#[test]
fn h17_one_generic_helper_from_two_envs() {
  let logger = Logger::default();
  let n = signup()
    .provide_env(hlist![logger.clone(), Db::with_rows("users", 3)])
    .block_on();
  assert_eq!(n.unwrap(), 3);
  let mailer = Mailer::default();
  notify()
    .provide_env(hlist![logger.clone(), mailer.clone()])
    .block_on()
    .unwrap();
  assert_eq!(logger.lines(), vec!["signup: 3 users", "notify: sent"]);
  assert_eq!(mailer.sent().len(), 1);
}
// corpus:end H17

// corpus:begin H18
fn cached_count() -> Effect<String, Never, caps!(Db, Cache)> {
  count_users().narrow().map(|n| n.to_string()).catch(|_| {
    effect!({
      let hit = ~Cache.get("users_count").unwrap_or_else(|| "0".to_string());
      format!("cached:{hit}")
    })
  })
}

#[test]
fn h18_recover_with_another_capability() {
  let healthy = hlist![Db::with_rows("users", 3), Cache::default()];
  assert_eq!(cached_count().provide_env(healthy).block_on().unwrap(), "3");
  let db = Db::default();
  db.set_failing(true);
  let cache = Cache::default();
  cache.put("users_count", "7");
  assert_eq!(
    cached_count()
      .provide_env(hlist![db, cache])
      .block_on()
      .unwrap(),
    "cached:7"
  );
}
// corpus:end H18

// corpus:begin H19
type ReportCaps = caps!(Db, Mailer);

fn report() -> Effect<(usize, ()), AppError, ReportCaps> {
  let count = count_users().narrow().map_err(AppError::from);
  let mail = effect!({ ~Mailer.send("ops@example.com", "report").await? });
  count.zip_par(mail)
}

#[test]
fn h19_parallel_fan_out() {
  let mailer = Mailer::default();
  let ok = hlist![Db::with_rows("users", 3), mailer.clone()];
  assert_eq!(report().provide_env(ok).block_on().unwrap(), (3, ()));
  let db = Db::default();
  db.set_failing(true);
  assert_eq!(
    report().provide_env(hlist![db, mailer]).block_on(),
    Err(Cause::Fail(AppError::Db(DbError("unavailable".into()))))
  );
}
// corpus:end H19

// corpus:begin H20
fn guarded_count(hang: bool) -> Effect<usize, AppError, caps!(Db)> {
  effect!({
    let db = ~Db;
    if hang { db.hang().await? } else { db.count("users").await? }
  })
  .timeout_fail(Duration::from_millis(50), || AppError::Timeout)
}

#[test]
fn h20_timeout_around_a_hanging_call() {
  let db = Db::with_rows("users", 3);
  assert_eq!(
    guarded_count(false).provide(db.clone()).block_on().unwrap(),
    3
  );
  let started = std::time::Instant::now();
  assert_eq!(
    guarded_count(true).provide(db).block_on(),
    Err(Cause::Fail(AppError::Timeout))
  );
  assert!(started.elapsed() < Duration::from_millis(500));
}
// corpus:end H20

// corpus:begin W01
fn base_layer(
  cfg: ConfigSource,
  logger: Logger,
) -> Layer<caps!(ConfigSource, Logger), AppError, caps!()> {
  Layer::succeed(hlist![cfg, logger])
}

type Infra = caps!(Db, Sessions, Cache);

fn infra_layer() -> Layer<Infra, AppError, caps!(ConfigSource)> {
  Layer::from_effect(effect!({
    let url = ~ConfigSource.get_string("db_url")?;
    let db = if url.starts_with("mem://") { Db::with_rows("users", 3) } else { Db::default() };
    hlist![db, Sessions::default(), Cache::default()]
  }))
}

type App = caps!(Greeting, Counter, Hits, Tenant, Mailer);

fn app_layer() -> Layer<App, AppError, caps!()> {
  Layer::succeed(hlist![
    Greeting("hello from effect-axum"),
    Counter(7),
    Hits::default(),
    Tenant("public".to_string()),
    Mailer::default(),
  ])
}

#[test]
fn w01_edge_wiring_of_the_showcase_env() {
  let cfg = ConfigSource::from_pairs([("db_url", "mem://app"), ("table", "users")]);
  let logger = Logger::default();
  let app = infra_layer()
    .provide_merge(base_layer(cfg, logger.clone()))
    .merge(app_layer());
  let env = app.build().block_on().unwrap();

  assert_eq!(
    hello().provide_env(env.clone()).block_on().unwrap(),
    "hello from effect-axum"
  );
  assert_eq!(
    user_count().provide_env(env.clone()).block_on().unwrap(),
    "3"
  );
  assert_eq!(counted().provide_env(env.clone()).block_on().unwrap(), 3);
  assert_eq!(
    summary().provide_env(env.clone()).block_on().unwrap(),
    "hello from effect-axum:3"
  );
  assert_eq!(signup().provide_env(env.clone()).block_on().unwrap(), 3);
  notify().provide_env(env.clone()).block_on().unwrap();
  assert_eq!(
    logger.lines(),
    vec![
      "counted 3",
      "summary for 3 users",
      "signup: 3 users",
      "notify: sent"
    ]
  );
}
// corpus:end W01
