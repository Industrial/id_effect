//! B risk R1 measured the other way: the request-scoped addition written **generically** over
//! the app env (a reusable auth middleware that does not know the concrete `AppEnv`), which is
//! what design §2.3 row 4 requires when `R` is generic. Not a corpus file: the corpus spells H10
//! the idiomatic way (`#[env(flatten)] app: AppEnv`, forward list 0). Count this file separately:
//! `bash spikes/scripts/count-annotations.sh spikes/kernel_b/tests/extension_generic.rs`.

use kernel_b::{Cause, Effect, Env, Exit, Has, effect};
use spike_corpus::{
  AppError, Cache, ConfigSource, Counter, CurrentUser, Db, Greeting, Hits, Logger, Mailer, Req,
  Sessions, Tenant,
};

/// H10's middleware, generic over the app env: forwards exactly what `whoami` reads.
#[derive(Clone, Env)]
struct WithUser<R> {
  #[env(base(forward(Tenant, Logger)))]
  base: R,
  user: CurrentUser,
}

fn whoami<R: Has<Tenant> + Has<Logger> + Has<CurrentUser>>() -> Effect<String, AppError, R> {
  effect!(|r| {
    let user = ~CurrentUser;
    ~Logger.info(format!("whoami {}", user.id));
    format!("{}@{}", user.id, ~Tenant.0)
  })
}

fn with_user<R: Env + Has<Tenant> + Has<Logger>>(app: &R, req: &Req) -> Exit<String, AppError> {
  let Some(id) = req.get_header("x-user") else {
    return Err(Cause::Fail(AppError::Unauthorized));
  };
  let env = WithUser {
    base: app.clone(),
    user: CurrentUser { id: id.to_string() },
  };
  whoami().provide(env).block_on()
}

#[derive(Clone, Env)]
struct SmallApp {
  tenant: Tenant,
  logger: Logger,
}

#[test]
fn generic_overlay_for_h10_needs_a_two_entry_forward_list() {
  let app = SmallApp {
    tenant: Tenant("acme".into()),
    logger: Logger::default(),
  };
  let req = Req::new("/").header("x-user", "u1");
  assert_eq!(with_user(&app, &req), Ok("u1@acme".to_string()));
}

/// The same overlay sized for the W01 showcase env: every capability any per-request handler
/// may read must be forwarded by name (10 entries), and adding a service to the app means
/// editing this list too.
/// (`rustfmt::skip` keeps the list on one line: `count-annotations.sh` counts `forward(..)` per
/// line.)
#[rustfmt::skip]
#[derive(Clone, Env)]
struct RequestOverlay<R> {
  #[env(base(forward(Greeting, Counter, Hits, Tenant, Logger, Sessions, Db, ConfigSource, Mailer, Cache)))]
  base: R,
  user: CurrentUser,
}

#[derive(Clone, Env)]
struct ShowcaseEnv {
  greeting: Greeting,
  counter: Counter,
  hits: Hits,
  tenant: Tenant,
  logger: Logger,
  sessions: Sessions,
  db: Db,
  config: ConfigSource,
  mailer: Mailer,
  cache: Cache,
}

#[test]
fn generic_overlay_for_the_showcase_env_needs_a_ten_entry_forward_list() {
  let app = ShowcaseEnv {
    greeting: Greeting("hi"),
    counter: Counter(1),
    hits: Hits::default(),
    tenant: Tenant("acme".into()),
    logger: Logger::default(),
    sessions: Sessions::default(),
    db: Db::with_rows("users", 3),
    config: ConfigSource::default(),
    mailer: Mailer::default(),
    cache: Cache::default(),
  };
  let env = RequestOverlay {
    base: app,
    user: CurrentUser { id: "u2".into() },
  };
  assert_eq!(whoami().provide(env).block_on(), Ok("u2@acme".to_string()));
}
