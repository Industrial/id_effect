//! The application: what every request shares ([`App`]), who is calling ([`Caller`]), and the
//! router with its middleware ([`router`]).

use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, HCons, HNil, fail, hlist};
use id_effect_kit::auth::oidc::OidcRoutes;
use id_effect_kit::auth::{
  Authenticate, ClaimMapping, CookieConfig, JwtIssuer, JwtKey, JwtVerifier, OidcClient, OidcConfig,
  PasswordHasher, PasswordParams, PgSessionStore, SessionConfig, Sessions, Totp, TotpConfig,
};
use id_effect_kit::authz::{AuthorizeExt, AuthzEvents, CurrentPrincipal, Guard, Policies};
use id_effect_kit::axum::middleware::{
  BodyLimitLayer, CompressionLayer, RateLimitKey, RateLimitLayer, RequestIdLayer, TimeoutLayer,
};
use id_effect_kit::axum::openapi::{Info, OpenApi, OpenApiUi, openapi_router};
use id_effect_kit::axum::route::{self, Endpoint, introspect};
use id_effect_kit::axum::{HealthIndicator, HealthRegistry, RequestCaps};
use id_effect_kit::redis::{RedisClient, RedisConfig, RedisRateLimitStore};
use id_effect_kit::sql::audit::{AuditContext, AuditLog};
use id_effect_kit::sql::{SqlClient, SqlClientService};

use crate::config::LedgerConfig;
use crate::db::{self, Database};
use crate::error::{ApiError, StartupError};
use crate::reports::{self, BalanceCache};
use crate::{accounts, auth, entries, policy};

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
}

impl Services {
  /// `database` alone: in-process cache and rate limit, no OpenID Connect, denials logged.
  pub fn new(database: Database) -> Self {
    Self {
      database,
      redis: None,
      oidc: None,
      events: denials_to_stderr(),
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

/// Opens what `config` names: the databases (migrated), Redis and the OpenID Connect provider.
pub fn start(config: &LedgerConfig) -> Effect<Services, StartupError> {
  let migrations = config.db.migrations.clone();
  let redis = config.redis.url.clone();
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
      let services = Services {
        redis,
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
    let totp = Totp::new(
      TotpConfig::new(settings.totp_issuer.clone()),
      auth::UserTotpSteps::new(Arc::clone(&db)),
    );
    Ok(Self {
      balances: reports::balance_cache(Arc::clone(&reads), services.redis.clone()),
      db,
      reads,
      sessions,
      hasher: PasswordHasher::new(params),
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
    let principal = caps
      .get::<CurrentPrincipal>()
      .map(|p| CurrentPrincipal::clone(&p))
      .filter(|p| p.is_authenticated())
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
/// (401 for an anonymous caller, 403 for a denied one). Every route of the app has one.
pub fn guarded(policies: &Policies, name: &str, endpoint: Endpoint<AppEnv>) -> Endpoint<AppEnv> {
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

/// The whole HTTP application for `config` over `services`.
///
/// Middleware, outermost first: request id (and span), compression, timeout, body limit,
/// authentication (session cookie or bearer API token), revoked-token check. Every route has a
/// guard naming its policy; `/openapi.json`, `/docs`, `/health` and the login routes are
/// `public`, and `/_routes` needs `routes.read`.
pub fn router(config: &LedgerConfig, services: Services) -> Result<axum::Router, StartupError> {
  let app = App::new(config, &services)?;
  let policies = app.policies.clone();
  let (rate, burst) = (config.auth.login_per_second, config.auth.login_burst);
  let login_limit = match &services.redis {
    Some(redis) => RateLimitLayer::new(RedisRateLimitStore::new(redis.clone(), rate, burst)),
    None => RateLimitLayer::per_second(rate, burst),
  }
  .key(client_address());
  let api = route::Router::<AppEnv>::new()
    .merge(auth::routes(&policies, login_limit))
    .merge(accounts::routes(&policies))
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
  let mut http = api
    .with_state(hlist![app.clone()])
    .into_axum()
    .merge(public(openapi_router(&doc, OpenApiUi::scalar())))
    .merge(public(health(&app).routes()))
    .merge(route_table);
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
  Ok(
    http
      .layer(axum::middleware::from_fn_with_state(
        app,
        auth::reject_revoked_tokens,
      ))
      .layer(authenticate)
      .layer(BodyLimitLayer::new(config.http.body_limit_bytes))
      .layer(TimeoutLayer::new(Duration::from_millis(
        config.http.request_timeout_ms,
      )))
      .layer(CompressionLayer::new())
      .layer(RequestIdLayer::new()),
  )
}

/// Who a login attempt counts against: the client address the reverse proxy in front of the
/// service reports (`X-Forwarded-For`, first hop; the proxy must overwrite, not append to, a
/// header the client sent), else the peer address of the connection, else one bucket shared by
/// every client the service cannot tell apart — so the limit always applies.
fn client_address() -> RateLimitKey {
  RateLimitKey::from_fn(|request| {
    let forwarded = request.headers().get("x-forwarded-for")?.to_str().ok()?;
    let client = forwarded.split(',').next()?.trim();
    (!client.is_empty()).then(|| format!("ip:{client}"))
  })
  .or(RateLimitKey::peer_ip())
  .or(RateLimitKey::from_fn(|_| Some("unidentified".to_owned())))
}

/// `/health` and `/ready` (the latter runs `SELECT 1` on the primary).
fn health(app: &App) -> HealthRegistry {
  let db = Arc::clone(&app.db);
  HealthRegistry::new().with_indicator(HealthIndicator::from_effect("db", move || {
    db.query("SELECT 1 AS up", &[]).map(|rows| rows.len())
  }))
}
