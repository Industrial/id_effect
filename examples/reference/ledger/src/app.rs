//! The application: what every request shares ([`App`]), who is calling ([`Caller`]), and the
//! router with its middleware ([`router`]).

use std::sync::Arc;
use std::time::Duration;

use axum::http::Method;
use axum::http::request::Parts;
use id_effect::{Effect, HCons, HNil, WallClock, fail, hlist};
use id_effect_kit::auth::oidc::OidcRoutes;
use id_effect_kit::auth::{
  Authenticate, ClaimMapping, CookieConfig, JwtIssuer, JwtKey, JwtVerifier, LoginThrottle,
  OidcClient, OidcConfig, PasswordHasher, PasswordParams, PgSessionStore, PgThrottleStore,
  SessionConfig, Sessions, Totp, TotpConfig,
};
use id_effect_kit::authz::{AuthorizeExt, AuthzEvents, CurrentPrincipal, Guard, Policies};
use id_effect_kit::axum::CapOverrideLayer;
use id_effect_kit::axum::middleware::{
  BodyLimitLayer, CompressionLayer, RateLimitKey, RequestIdLayer, TimeoutLayer,
};
use id_effect_kit::axum::openapi::{Info, OpenApi, OpenApiUi, openapi_router};
use id_effect_kit::axum::route::{
  self, AccessLayer, AccessReport, Endpoint, Guarded, RouteAccess, RouteInfo, RouteRegistry,
  introspect,
};
use id_effect_kit::axum::server::security::{CookieKey, CsrfToken, SecurityLayer};
use id_effect_kit::axum::{
  ClientIp, HealthIndicator, HealthRegistry, HostConfig, RequestCaps, TrustedProxies,
};
use id_effect_kit::config::ProfileWatch;
use id_effect_kit::console::EnvFilterHandle;
use id_effect_kit::jobs::PgJobQueue;
use id_effect_kit::mail::{MailerService, TestMailbox};
use id_effect_kit::messaging::{MessageBroker, Topic};
use id_effect_kit::posture::{Posture, Surface};
use id_effect_kit::redis::{RedisClient, RedisConfig, RedisStreamsBroker, RedisStreamsConfig};
use id_effect_kit::sql::audit::{AuditContext, AuditLog};
use id_effect_kit::sql::{SqlClient, SqlClientService};
use id_effect_kit::storage::{MemoryStore, ObjectStorage, S3Config, S3Store};

use crate::closure::{self, Closures};
use crate::config::{LedgerConfig, StorageConfig};
use crate::db::{self, Database};
use crate::error::{ApiError, StartupError};
use crate::limits::{self, LimitSource, LimitsFile, TenantHint};
use crate::mail::{self, Mail};
use crate::reports::{self, BalanceCache};
use crate::{accounts, auth, console, entries, jobs, notifications, observability, policy, signup};

/// The router state: one [`App`] in a capability list, so handlers lift environment-free
/// effects into it with `narrow()`.
pub type AppEnv = HCons<App, HNil>;

/// Everything the handlers share. Cheap to clone.
#[derive(Clone)]
pub struct App {
  /// The primary database.
  pub db: Arc<dyn SqlClient>,
  /// Reads that may lag: the replicas (or the primary when there are none).
  pub reads: Arc<dyn SqlClient>,
  /// Server-side sessions in PostgreSQL.
  pub sessions: Sessions,
  /// Argon2id password hashing.
  pub hasher: PasswordHasher,
  /// Backoff after failed password logins, per account and per client address (counted in
  /// PostgreSQL: every instance sees every failure).
  pub throttle: LoginThrottle,
  /// Signs API tokens.
  pub tokens: JwtIssuer,
  /// Checks API tokens (also used by the authentication layer).
  pub verifier: JwtVerifier,
  /// TOTP second factor.
  pub totp: Totp,
  /// The named policies.
  pub policies: Policies,
  /// The audit log of postings.
  pub audit: AuditLog,
  /// Account balances, cached in Redis (or in process).
  pub balances: BalanceCache,
  /// How long an API token lives.
  pub token_ttl: Duration,
  /// Where postings are announced (the outbox's topic).
  pub topic: Topic,
  /// The mail transport, templates and sender.
  pub mail: Mail,
  /// Where the statements are stored.
  pub storage: ObjectStorage,
  /// The job queue (`id_effect_jobs` on the primary).
  pub jobs: PgJobQueue,
  /// Account closures, a durable workflow on the primary ([`crate::closure`]).
  pub closures: Closures,
}

/// What the service runs on, opened by [`start`] (or by a test).
pub struct Services {
  /// The primary and the read replicas.
  pub database: Database,
  /// Shared cache and rate limits; in process without it.
  pub redis: Option<RedisClient>,
  /// OpenID Connect login, when configured.
  pub oidc: Option<OidcClient>,
  /// Where authorization decisions are recorded.
  pub events: AuthzEvents,
  /// Where mail goes.
  pub mailer: MailerService,
  /// Where the statements are stored.
  pub storage: ObjectStorage,
  /// Where the relay publishes the postings' announcements (Redis Streams); none: they stay
  /// in the outbox.
  pub broker: Option<Arc<dyn MessageBroker>>,
  /// The login limits, reloaded from the profile file; none: fixed at start.
  pub limits: Option<ProfileWatch<LimitsFile>>,
  /// The reloadable log filter the console changes (from the OpenTelemetry starter).
  pub env_filter: Option<EnvFilterHandle>,
}

impl Services {
  /// `database` alone: in-process cache, rate limit, mail and storage, no broker, no OpenID
  /// Connect, denials logged.
  pub fn new(database: Database) -> Self {
    Self {
      database,
      redis: None,
      oidc: None,
      events: denials_to_stderr(),
      mailer: MailerService::new(TestMailbox::new()),
      storage: ObjectStorage::new(MemoryStore::new()),
      broker: None,
      limits: None,
      env_filter: None,
    }
  }
}

/// Every denied decision as one JSON line on stderr (the deployment ships stderr to its logs).
fn denials_to_stderr() -> AuthzEvents {
  AuthzEvents::from_fn(|event| {
    if !event.is_allowed() {
      eprintln!(
        "{}",
        serde_json::to_string(event).unwrap_or_else(|e| format!("authz event: {e}"))
      );
    }
  })
}

/// Opens what `config` names: the databases (migrated), Redis (with the Redis Streams broker),
/// the mail transport, the statements bucket and the OpenID Connect provider.
pub fn start(config: &LedgerConfig) -> Effect<Services, StartupError> {
  let migrations = config.db.migrations.clone();
  let redis = config.redis.url.clone();
  let mailer = match mail::mailer(&config.mail, mail::http_client(&config.outbound)) {
    Ok(mailer) => mailer,
    Err(e) => return fail(e),
  };
  let storage = match statement_store(&config.storage) {
    Ok(storage) => storage,
    Err(e) => return fail(e),
  };
  let consumer = config.messaging.consumer.clone();
  let oidc = config.oidc.clone().map(|oidc| {
    OidcConfig::new(oidc.issuer, oidc.client_id, oidc.redirect_url)
      .client_secret(oidc.client_secret.expose().clone())
      .claims(ClaimMapping::default().subject("preferred_username"))
  });
  Database::connect(&config.db)
    .flat_map(move |database| {
      db::migrate(Arc::clone(&database.primary), migrations).map(move |_applied| database)
    })
    .flat_map(move |database| match redis {
      Some(url) => RedisClient::connect(RedisConfig::new(url))
        .map_err(StartupError::from)
        .map(move |redis| (database, Some(redis))),
      None => id_effect::succeed((database, None)),
    })
    .flat_map(move |(database, redis)| {
      let broker = redis.clone().map(|redis| {
        let config = RedisStreamsConfig::new().consumer(consumer);
        Arc::new(RedisStreamsBroker::new(redis, config)) as Arc<dyn MessageBroker>
      });
      let services = Services {
        redis,
        mailer,
        storage,
        broker,
        ..Services::new(database)
      };
      match oidc {
        Some(oidc) => OidcClient::discover(oidc)
          .map_err(StartupError::from)
          .map(move |client| Services {
            oidc: Some(client),
            ..services
          }),
        None => id_effect::succeed(services),
      }
    })
}

/// The statements bucket of `config` (S3, with the environment's AWS credentials unless the
/// profile names some), or a store in memory without one.
pub fn statement_store(config: &StorageConfig) -> Result<ObjectStorage, StartupError> {
  let Some(bucket) = &config.bucket else {
    return Ok(ObjectStorage::new(MemoryStore::new()));
  };
  let mut s3 = S3Config::from_env(bucket.clone()).with_region(config.region.clone());
  if let Some(endpoint) = &config.endpoint {
    s3 = s3.with_endpoint(endpoint.clone());
  }
  if let (Some(id), Some(secret)) = (&config.access_key_id, &config.secret_access_key) {
    s3 = s3.with_credentials(id.clone(), secret.expose().clone());
  }
  S3Store::new(&s3.with_allow_http(config.allow_http))
    .map(ObjectStorage::new)
    .map_err(|e| StartupError::Settings(format!("storage: {e}")))
}

impl App {
  /// The shared state for `config` over `services`.
  pub fn new(config: &LedgerConfig, services: &Services) -> Result<Self, StartupError> {
    let settings = &config.auth;
    let db = Arc::clone(&services.database.primary);
    let reads = Arc::clone(&services.database.reads);
    let params = PasswordParams::new(settings.password_memory_kib, 2, 1)
      .map_err(|e| StartupError::Settings(e.to_string()))?;
    let cookie = CookieConfig {
      name: settings.session_cookie.clone(),
      secure: settings.secure_cookies,
      ..CookieConfig::default()
    };
    let sessions = Sessions::new(
      PgSessionStore::from_service(SqlClientService(Arc::clone(&db))),
      SessionConfig {
        cookie,
        ..SessionConfig::default()
      },
    );
    let secret = settings.jwt_secret.expose().as_bytes();
    let token_ttl = Duration::from_secs(settings.token_ttl_minutes * 60);
    let tokens = JwtIssuer::hs256(secret)
      .issuer(settings.jwt_issuer.clone())
      .audience(settings.jwt_audience.clone())
      .ttl(token_ttl);
    let verifier = JwtVerifier::new([JwtKey::hmac(secret)])
      .issuer(settings.jwt_issuer.clone())
      .audience(settings.jwt_audience.clone())
      .require_claim("jti");
    // Failed logins and wrong TOTP codes counted on the primary, shared by every instance.
    let failures = PgThrottleStore::from_service(SqlClientService(Arc::clone(&db)));
    let totp = Totp::with_stores(
      TotpConfig::new(settings.totp_issuer.clone()),
      auth::UserTotpSteps::new(Arc::clone(&db)),
      failures.clone(),
    );
    let (topic, _group) = notifications::names(&config.messaging)?;
    let mail = Mail {
      mailer: services.mailer.clone(),
      templates: mail::templates(&config.mail)?,
      from: mail::sender(&config.mail)?,
    };
    let closures = Closures::new(&db, WallClock::system())
      .map_err(|e| StartupError::Settings(format!("workflows: {e}")))?;
    Ok(Self {
      closures,
      balances: reports::balance_cache(Arc::clone(&reads), services.redis.clone()),
      jobs: jobs::queue(&db),
      topic,
      mail,
      storage: services.storage.clone(),
      db,
      reads,
      sessions,
      hasher: PasswordHasher::new(params),
      throttle: LoginThrottle::new(failures).free_attempts(settings.login_free_attempts),
      tokens,
      verifier,
      totp,
      policies: policy::policies(services.events.clone()),
      audit: AuditLog::new(),
      token_ttl,
    })
  }
}

/// The request's caller: an authenticated principal and the organisation whose ledger it acts
/// on. Built from the request capabilities the authentication layer recorded.
#[derive(Clone)]
pub struct Caller {
  principal: CurrentPrincipal,
  org: String,
}

impl Caller {
  /// The caller of the request whose capabilities are `caps`: 401 without a principal, 403
  /// for a principal of no organisation (fail closed: nothing runs unscoped).
  pub fn of(caps: &RequestCaps) -> Result<Self, ApiError> {
    Self::new(caps.get::<CurrentPrincipal>().as_deref())
  }

  /// The caller `principal` (the one the authentication layer recorded): 401 without one or for
  /// the anonymous one, 403 for a principal of no organisation. RPC procedures find it in their
  /// environment ([`crate::rpc`]).
  pub fn new(principal: Option<&CurrentPrincipal>) -> Result<Self, ApiError> {
    let principal = principal
      .filter(|p| p.is_authenticated())
      .cloned()
      .ok_or(ApiError::Unauthenticated)?;
    let org = principal.tenant().ok_or(ApiError::NoTenant)?.to_owned();
    Ok(Self { principal, org })
  }

  /// The principal's id (a username, or the IdP's `preferred_username`).
  pub fn id(&self) -> &str {
    self.principal.id()
  }

  /// The organisation (the tenant every repository is scoped to).
  pub fn org(&self) -> &str {
    &self.org
  }

  /// The principal, for policies.
  pub fn principal(&self) -> &CurrentPrincipal {
    &self.principal
  }

  /// Actor and tenant of the audit records this caller writes.
  pub fn audit(&self) -> AuditContext {
    AuditContext::for_tenant(self.id(), self.org.clone())
  }
}

impl Caller {
  /// Fails with the policy's 401 / 403 unless `policies` allow this caller `name` on `resource`.
  pub fn authorize<Res: 'static>(
    &self,
    policies: &Policies,
    name: &str,
    resource: &Res,
  ) -> Effect<(), ApiError> {
    policies
      .authorize(name, &self.principal, resource)
      .map_err(ApiError::from)
  }
}

/// `endpoint` behind a guard evaluating the registry policy `name` before the handler runs
/// (401 for an anonymous caller, 403 for a denied one). Every typed route of the app has one,
/// except the public sign-in routes.
pub fn guarded(
  policies: &Policies,
  name: &str,
  endpoint: Endpoint<AppEnv>,
) -> Endpoint<AppEnv, Guarded> {
  endpoint.authorize(Guard::named(policies, name)).into()
}

/// Runs `f` for the request's caller and lifts its effect into the router's environment; with no
/// caller it answers 401 / 403 without running anything.
pub fn as_caller<A: Send + 'static>(
  caps: &RequestCaps,
  f: impl FnOnce(Caller) -> Effect<A, ApiError>,
) -> Effect<A, ApiError, AppEnv> {
  match Caller::of(caps) {
    Ok(caller) => f(caller).narrow(),
    Err(e) => fail(e),
  }
}

/// The whole HTTP application for `config` over `services` ([`assemble`]'s router).
pub fn router(config: &LedgerConfig, services: Services) -> Result<axum::Router, StartupError> {
  assemble(config, services).map(|assembled| assembled.router)
}

/// The Ledger's HTTP application and what it declares about itself: every route it serves with
/// its access, and how its operator surfaces are served (the startup posture check, [`posture`]).
pub struct Assembled {
  /// The whole HTTP application.
  pub router: axum::Router,
  /// Every route the router serves: the typed routes, then the plain ones ([`plain_routes`]).
  /// Any other route answers `404` ([`declared_only`]).
  pub routes: RouteRegistry,
  /// The console: guarded where `console.enabled` (dev, the tests), absent otherwise (prod).
  pub console: Surface,
  /// `/docs` and `/openapi.json`: public in `dev` and `test`, not mounted in any other profile.
  pub docs: Surface,
}

impl Assembled {
  /// Who may call each route ([`Assembled::routes`]); printed at startup.
  pub fn access_report(&self) -> AccessReport {
    AccessReport::new(self.routes.iter(), false)
  }
}

/// The whole HTTP application for `config` over `services`, with the routes it declares.
///
/// Middleware, outermost first: request id (and span), compression, timeout, body limit, the
/// organisation a login names ([`TenantHint`]), authentication (session cookie or bearer API
/// token), revoked-token check, the deny-by-default access layer ([`declared_only`]: a route
/// neither typed nor in [`plain_routes`] answers `404`), and innermost the security layer
/// ([`SecurityLayer::recommended`]: CSRF, CSP with nonces, the security headers; see
/// [`secured`]).
/// Every route has a guard naming its policy; `/openapi.json` and `/docs` (dev and test only),
/// `/health`, `/ready`, the login, account and CSRF routes (and `/_mail/previews` in dev) are
/// `public`, `/_routes` needs `routes.read` and `/_console` `console.admin`.
pub fn assemble(config: &LedgerConfig, services: Services) -> Result<Assembled, StartupError> {
  let app = App::new(config, &services)?;
  let policies = app.policies.clone();
  let source = match services.limits.clone() {
    Some(watch) => LimitSource::Watched(watch),
    None => LimitSource::Fixed(config.auth.limits()),
  };
  let trusted = TrustedProxies::new(&config.http.trusted_proxies)
    .map_err(|e| StartupError::Settings(format!("http.trusted_proxies: {e}")))?;
  let login_limit = limits::login_limit(
    source,
    services.redis.clone(),
    client_address(trusted.clone()),
  );
  let api = route::Router::<AppEnv>::new()
    .merge(auth::routes(&policies, login_limit.clone()))
    .merge(accounts::routes(&policies))
    .merge(closure::routes(&policies))
    .merge(entries::routes(&policies))
    .merge(reports::routes(&policies));

  let mut doc = OpenApi::from_router(&api)
    .info(Info::new("Ledger", env!("CARGO_PKG_VERSION")))
    .bearer_jwt("bearer")
    .cookie_session("session", config.auth.session_cookie.clone());
  if let Some(oidc) = &config.oidc {
    let discovery = format!("{}/.well-known/openid-configuration", oidc.issuer);
    doc = doc.openid_connect("oidc", discovery);
  }
  let public = |router: axum::Router| router.layer(Guard::named(&policies, "public"));
  let route_table =
    introspect::routes_router(api.registry()).layer(Guard::named(&policies, "routes.read"));
  let flows = signup::accounts(&app, mail::account_mails(&app.mail, &config.mail));
  let csrf = axum::Router::new().route("/auth/csrf", axum::routing::get(csrf_token));
  let health = health(&app);
  // `/docs` and `/openapi.json` are served unguarded in `dev` and `test` only (ADR 0013 R7).
  let ui = OpenApiUi::scalar().profile(config.profile.as_str());
  let docs = if ui.is_mounted() {
    Surface::Open
  } else {
    Surface::Absent
  };
  let console = if config.console.enabled {
    Surface::Guarded
  } else {
    Surface::Absent
  };
  // The accounts and journal over RPC, beside REST (`crate::rpc`).
  let rpc = crate::rpc::mount(Arc::new(app.clone()), &policies);
  let mut routes = api.registry().clone();
  routes.extend(rpc.routes);
  routes.extend(plain_routes(
    config,
    docs == Surface::Open,
    services.oidc.is_some(),
  ));
  let mut http = api
    .with_state(hlist![app.clone()])
    .into_axum()
    .merge(rpc.router)
    .merge(public(health.clone().routes()))
    .merge(public(signup::routes(flows).layer(login_limit.client)))
    .merge(public(csrf))
    .merge(route_table);
  // The HTML pages carry inline styles and scripts: their security layer sends no CSP.
  let mut pages = public(openapi_router(&doc, ui));
  if config.mail.previews {
    pages = pages.merge(public(mail::previews(&app.mail)));
  }
  if config.console.enabled {
    pages = pages.merge(console::router(
      &policies,
      health,
      app.jobs.clone(),
      services.env_filter.clone(),
    ));
  }
  if let Some(oidc) = services.oidc {
    // The login flows (state, nonce, PKCE verifier) live in PostgreSQL too, so a callback can
    // land on any instance. Over plain HTTP (dev) the flow cookie cannot be `__Host-`.
    let mut flow = OidcRoutes::flow_session_config();
    if !config.auth.secure_cookies {
      flow.cookie.name = "id_oidc_flow".to_owned();
      flow.cookie.secure = false;
    }
    let flows = Sessions::new(
      PgSessionStore::from_service(SqlClientService(Arc::clone(&app.db))),
      flow,
    );
    let routes = OidcRoutes::new(oidc, app.sessions.clone())
      .flow_sessions(flows)
      .after_login("/me");
    http = http.merge(public(routes.router()));
  }
  let authenticate = Authenticate::session(app.sessions.clone())
    .with_bearer_jwt(app.verifier.clone())
    .realm("ledger");
  let key = CookieKey::from_base64(config.auth.cookie_key.expose())
    .map_err(|e| StartupError::Settings(format!("auth.cookie_key: {e}")))?;
  let router = declared_only(secured(http, pages, &key), &routes)
    .layer(axum::middleware::from_fn_with_state(
      app.clone(),
      auth::reject_revoked_tokens,
    ))
    .layer(authenticate)
    .layer(CapOverrideLayer::new(tenant_hint))
    .layer(CapOverrideLayer::new(move |parts: &Parts| {
      // The login throttle's client address (the same one the login limit counts).
      ClientIp::from_parts(&parts.extensions, &parts.headers, &trusted)
    }))
    .layer(BodyLimitLayer::new(config.http.body_limit_bytes))
    .layer(TimeoutLayer::new(Duration::from_millis(
      config.http.request_timeout_ms,
    )))
    .layer(CompressionLayer::new())
    .layer(RequestIdLayer::new());
  Ok(Assembled {
    router,
    routes,
    console,
    docs,
  })
}

/// `app` serving only the routes `declared` lists (method and path template): any other route
/// merged into it answers `404` Problem Details (deny by default, finding H8). Applied after the
/// last route, as a route layer, so it sees the matched route.
pub fn declared_only(app: axum::Router, declared: &RouteRegistry) -> axum::Router {
  app.route_layer(AccessLayer::new(declared.iter()))
}

/// The plain Axum routes [`assemble`] merges next to the typed ones, each with the access of the
/// guard it sits behind there: the health checks, the account flows (`/auth/register`, …), the
/// CSRF token and, when `docs`, `/openapi.json` and `/docs` are public; `/_routes` is guarded by
/// `routes.read`; with `config` enabling them, the mail previews are public and the console is
/// guarded by `console.admin`; with `oidc`, the three OpenID Connect routes are public.
pub fn plain_routes(config: &LedgerConfig, docs: bool, oidc: bool) -> Vec<RouteInfo> {
  let declare = |method: Method, path: &str, policy: Option<&str>| {
    let mut info = RouteInfo::new(method, path);
    match policy {
      Some(policy) => {
        info.access = RouteAccess::Guarded;
        info.policies.push(policy.to_owned());
      }
      None => info.access = RouteAccess::Public,
    }
    info
  };
  let public = |method: Method, path: &str| declare(method, path, None);
  let mut routes = vec![
    public(Method::GET, "/health"),
    public(Method::GET, "/ready"),
  ];
  for flow in [
    "register",
    "verify",
    "verify/resend",
    "password/forgot",
    "password/reset",
  ] {
    routes.push(public(Method::POST, &format!("/auth/{flow}")));
  }
  routes.push(public(Method::GET, "/auth/csrf"));
  routes.push(declare(Method::GET, "/_routes", Some("routes.read")));
  if docs {
    routes.push(public(Method::GET, "/openapi.json"));
    routes.push(public(Method::GET, "/docs"));
  }
  if config.mail.previews {
    for path in [
      "/_mail/previews",
      "/_mail/previews/{name}",
      "/_mail/previews/{name}/{view}",
    ] {
      routes.push(public(Method::GET, path));
    }
  }
  if config.console.enabled {
    let console = |method: Method, path: &str| declare(method, path, Some("console.admin"));
    routes.push(console(Method::GET, "/_console"));
    for api in [
      "fibers",
      "supervisors",
      "metrics",
      "health",
      "log-level",
      "jobs",
    ] {
      routes.push(console(Method::GET, &format!("/_console/api/{api}")));
    }
    routes.push(console(Method::PUT, "/_console/api/log-level"));
    for action in ["retry", "cancel"] {
      routes.push(console(
        Method::POST,
        &format!("/_console/api/jobs/{{id}}/{action}"),
      ));
    }
  }
  if oidc {
    routes.push(public(Method::GET, "/auth/oidc/login"));
    routes.push(public(Method::GET, "/auth/oidc/callback"));
    routes.push(public(Method::POST, "/auth/oidc/logout"));
  }
  routes
}

/// The server settings of `config`: its address, a 20 s drain on shutdown and the default
/// in-flight cap (load shedding). Plain HTTP on TCP only: the Ledger runs behind a load balancer
/// that terminates TLS (and HTTP/3), so it serves no HTTP/3 of its own.
pub fn host_config(config: &LedgerConfig) -> HostConfig {
  HostConfig {
    bind_host: config.http.host.clone(),
    bind_port: config.http.port,
    shutdown_timeout: Duration::from_secs(20),
    http3: false,
    ..HostConfig::default()
  }
}

/// What the Ledger states about itself for the startup posture check (ADR 0013 R10), on top of
/// `base` (the profile and the risks its configuration accepts): the console and `/docs` as
/// `app` serves them, CSRF on ([`SecurityLayer::recommended`] in [`secured`]), no mesh, the
/// redaction policy of `config` and the in-flight cap of [`host_config`]. `main.rs` checks it
/// once the app is assembled and refuses to start the `prod` profile on a failed probe.
pub fn posture(config: &LedgerConfig, app: &Assembled, base: Posture) -> Posture {
  base
    .console(app.console)
    .docs(app.docs)
    .csrf(true)
    .no_mesh()
    .redaction(&observability::redaction(config))
    .host(&host_config(config))
    // Every RPC procedure declares its access (C1); `/rpc` itself is in the access report.
    .rpc_service(&crate::rpc::LedgerService::contract())
    .access_report(&app.access_report())
}

/// `api` and `pages` under [`SecurityLayer::recommended`] (`pages` without a CSP: they carry
/// inline styles and scripts).
///
/// A request authenticated by a session cookie, or anonymous, must echo the CSRF token of its
/// `__Host-csrf` cookie in `X-CSRF-Token` on every unsafe method (`GET /auth/csrf` issues both).
/// A request carrying only a bearer token (no cookie at all) runs without the check
/// ([`SecurityLayer::csrf_exempt_bearer`]): nothing ambient authenticates it, so another site
/// cannot forge it.
pub fn secured(api: axum::Router, pages: axum::Router, key: &CookieKey) -> axum::Router {
  let layer = |csp: bool| {
    // `/rpc` needs no CSRF token: an RPC POST carries `x-rpc: 1` and a JSON content type, which
    // a cross-site form cannot send (ADR 0014 C5).
    let layer = SecurityLayer::recommended(key.clone())
      .csrf_exempt_bearer()
      .csrf_exempt(crate::rpc::RPC_PATH);
    if csp { layer } else { layer.without_csp() }
  };
  api.layer(layer(true)).merge(pages.layer(layer(false)))
}

/// `GET /auth/csrf`: the session's CSRF token (the security layer sets its cookie on the first
/// request), to echo in `X-CSRF-Token`.
async fn csrf_token(token: CsrfToken) -> axum::Json<serde_json::Value> {
  axum::Json(serde_json::json!({ "token": token.as_str() }))
}

/// The organisation a request names in `X-Ledger-Org`, for the per-organisation login limit.
fn tenant_hint(parts: &Parts) -> Option<TenantHint> {
  let value = parts.headers.get(limits::TENANT_HEADER)?.to_str().ok()?;
  TenantHint::parse(value)
}

/// Who a login attempt counts against: the [`ClientIp`](id_effect_kit::axum::ClientIp) — the
/// connection's peer or, when that is one of the `trusted` reverse proxies (`http.trusted_proxies`,
/// none by default), the rightmost `X-Forwarded-For` entry they did not write themselves, so a
/// client cannot pick its own bucket by sending the header — else one bucket shared by every
/// client the service cannot tell apart, so the limit always applies.
fn client_address(trusted: TrustedProxies) -> RateLimitKey {
  RateLimitKey::client_ip(trusted).or(RateLimitKey::from_fn(|_| Some("unidentified".to_owned())))
}

/// `/health` and `/ready` (the latter runs `SELECT 1` on the primary).
fn health(app: &App) -> HealthRegistry {
  let db = Arc::clone(&app.db);
  HealthRegistry::new().with_indicator(HealthIndicator::from_effect("db", move || {
    db.query("SELECT 1 AS up", &[]).map(|rows| rows.len())
  }))
}
