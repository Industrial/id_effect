//! Tenants, their databases and who may do what (ADR 0015 D10).
//!
//! - **One tenant, from the principal.** Requests carry a bearer JWT ([`TokenKeys::layer`]);
//!   its `tenant` claim is the only source of the tenant (`TenantResolver`'s default, no Host or
//!   header), and the kit's `TenancyLayer` turns it into both the authorization tenant and the SQL
//!   tenant of the request. Jobs carry the tenant the app wrote into them ([`Tenancy::new`]).
//! - **One database per tenant where configured.** [`Tenants::router`] maps each tenant to its
//!   database (`acme` shares the main database, `globex` has its own), as a `TenantRouter`: a
//!   tenant the configuration does not name has no connection at all (fail closed), and at most
//!   64 tenant pools are open. Rows are still filtered by tenant inside every database.
//! - **Roles.** `editor` ingests, `reviewer` publishes, both read; `agent` is an outside MCP
//!   agent. Every guard also requires a tenant claim ([`guard`]).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Ctx, DynEnv, Effect};
use id_effect_kit::auth::{Authenticate, JwtIssuer, JwtKey, JwtVerifier};
use id_effect_kit::authz::{CurrentPrincipal, Guard, PolicyExt, predicate, require_any_role};
use id_effect_kit::i18n::Locale;
use id_effect_kit::sql::pg::PgPoolConfig;
use id_effect_kit::tenancy::{Tenancy, TenantId, TenantRouter};

use crate::domain::PipelineError;

/// Ingests documents and reads them.
pub const ROLE_EDITOR: &str = "editor";
/// Publishes reviewed documents and reads them.
pub const ROLE_REVIEWER: &str = "reviewer";
/// An outside agent using the MCP server.
pub const ROLE_AGENT: &str = "agent";

/// The audience every Pipeline token names.
pub const AUDIENCE: &str = "pipeline";

/// The shortest HMAC secret accepted (256 bits, HS256).
pub const MIN_SECRET_BYTES: usize = 32;

/// The longest tenant id accepted in the configuration.
pub const MAX_TENANT_ID_LEN: usize = 63;

/// One tenant: its id, its locale (extraction labels, MCP tool descriptions) and its own
/// database, if it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantConfig {
  /// The tenant id (the `tenant` claim): lowercase ASCII letters, digits and `-`.
  pub id: String,
  /// Its locale (`en`, `de`).
  pub locale: Locale,
  /// Its own database URL; `None` shares the main database.
  pub database_url: Option<String>,
}

impl TenantConfig {
  /// The tenant `id` in `locale` on the main database.
  ///
  /// # Errors
  ///
  /// When `id` is not a valid tenant id or `locale` not a locale.
  pub fn new(id: &str, locale: &str) -> Result<Self, String> {
    let valid = !id.is_empty()
      && id.len() <= MAX_TENANT_ID_LEN
      && id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !valid {
      return Err(format!(
        "tenant id {id:?}: lowercase letters, digits and '-' only"
      ));
    }
    let locale = Locale::parse(locale).map_err(|e| format!("tenant {id}: locale: {e}"))?;
    Ok(Self {
      id: id.to_owned(),
      locale,
      database_url: None,
    })
  }

  /// The tenant on a database of its own at `url`.
  #[must_use]
  pub fn with_database(mut self, url: impl Into<String>) -> Self {
    self.database_url = Some(url.into());
    self
  }
}

/// Every configured tenant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tenants {
  tenants: BTreeMap<String, TenantConfig>,
}

impl Tenants {
  /// The tenants `list` (a later entry with the same id replaces an earlier one).
  pub fn new(list: impl IntoIterator<Item = TenantConfig>) -> Self {
    Self {
      tenants: list.into_iter().map(|t| (t.id.clone(), t)).collect(),
    }
  }

  /// The tenant `id`, if configured.
  pub fn get(&self, id: &str) -> Option<&TenantConfig> {
    self.tenants.get(id)
  }

  /// Every tenant, by id.
  pub fn iter(&self) -> impl Iterator<Item = &TenantConfig> {
    self.tenants.values()
  }

  /// The locale of tenant `id` (`en` for an unknown one).
  pub fn locale(&self, id: &str) -> Locale {
    self
      .get(id)
      .map(|t| t.locale.clone())
      .unwrap_or_else(|| Locale::parse("en").expect("en is a locale"))
  }

  /// The database URL of tenant `t` (`main_url` when it has none of its own).
  pub fn database_url<'a>(&'a self, t: &'a TenantConfig, main_url: &'a str) -> &'a str {
    t.database_url.as_deref().unwrap_or(main_url)
  }

  /// The distinct database URLs (the main one first): what migrations run on.
  pub fn database_urls(&self, main_url: &str) -> Vec<String> {
    let mut urls = vec![main_url.to_owned()];
    for t in self.iter() {
      let url = self.database_url(t, main_url).to_owned();
      if !urls.contains(&url) {
        urls.push(url);
      }
    }
    urls
  }

  /// The tenant router: one pool of `max_connections` per tenant, on its database (or the main
  /// one). Unknown tenants have no connection.
  pub fn router(&self, main_url: &str, max_connections: u32) -> TenantRouter {
    TenantRouter::from_pg_configs(self.iter().map(|t| {
      (
        t.id.clone(),
        PgPoolConfig::from_url(self.database_url(t, main_url)).with_max_size(max_connections),
      )
    }))
  }
}

/// How tokens are checked: an HS256 secret and the issuer.
#[derive(Clone)]
pub struct TokenKeys {
  secret: Arc<[u8]>,
  issuer: String,
}

impl std::fmt::Debug for TokenKeys {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("TokenKeys")
      .field("issuer", &self.issuer)
      .finish_non_exhaustive()
  }
}

impl TokenKeys {
  /// Tokens signed with `secret` (at least [`MIN_SECRET_BYTES`]) by `issuer`.
  ///
  /// # Errors
  ///
  /// When the secret is shorter than [`MIN_SECRET_BYTES`] or the issuer is empty.
  pub fn new(secret: impl Into<Vec<u8>>, issuer: impl Into<String>) -> Result<Self, String> {
    let secret = secret.into();
    let issuer = issuer.into();
    if secret.len() < MIN_SECRET_BYTES {
      return Err(format!(
        "the token secret must be at least {MIN_SECRET_BYTES} bytes"
      ));
    }
    if issuer.trim().is_empty() {
      return Err("the token issuer must not be empty".to_owned());
    }
    Ok(Self {
      secret: secret.into(),
      issuer,
    })
  }

  /// The verifier: HS256 with this secret, this issuer, audience [`AUDIENCE`], `exp` required.
  pub fn verifier(&self) -> JwtVerifier {
    JwtVerifier::new([JwtKey::hmac(&self.secret)])
      .issuer(self.issuer.clone())
      .audience(AUDIENCE)
      .require_exp(true)
  }

  /// An issuer of tokens these keys verify, valid for `ttl` (tests, and a sign-in service).
  pub fn issuer(&self, ttl: Duration) -> JwtIssuer {
    JwtIssuer::hs256(&self.secret)
      .issuer(self.issuer.clone())
      .audience(AUDIENCE)
      .ttl(ttl)
  }

  /// The authentication layer: bearer JWTs only, realm `pipeline`.
  pub fn layer(&self) -> Authenticate {
    Authenticate::bearer_jwt(self.verifier()).realm(AUDIENCE)
  }
}

/// The guard `name`: a principal holding one of `roles` with a tenant claim (401 for an
/// anonymous caller, 403 otherwise).
pub fn guard(name: &str, roles: &[&str]) -> Guard {
  let has_tenant = predicate(
    "the principal belongs to no tenant",
    |p: &CurrentPrincipal, _: &()| p.tenant().is_some_and(|t| !t.is_empty()),
  );
  Guard::new(
    name,
    require_any_role(roles.iter().copied()).and(has_tenant),
  )
}

/// The guard of reads: an editor or a reviewer.
pub fn read_guard() -> Guard {
  guard("pipeline.read", &[ROLE_EDITOR, ROLE_REVIEWER])
}

/// The guard of ingestion: an editor.
pub fn ingest_guard() -> Guard {
  guard("pipeline.ingest", &[ROLE_EDITOR])
}

/// The guard of publication: a reviewer.
pub fn publish_guard() -> Guard {
  guard("pipeline.publish", &[ROLE_REVIEWER])
}

/// The guard of the MCP endpoint of `tenant`: an agent whose tenant claim is `tenant`.
pub fn agent_guard(tenant: &str) -> Guard {
  let tenant = tenant.to_owned();
  let same = predicate(
    "the agent belongs to another tenant",
    move |p: &CurrentPrincipal, _: &()| p.tenant() == Some(tenant.as_str()),
  );
  Guard::new("pipeline.mcp", require_any_role([ROLE_AGENT]).and(same))
}

/// The tenant of an RPC call: the [`TenantId`] the `TenancyLayer` resolved from the principal
/// and the transport carried into the call's environment.
pub fn call_tenancy<E: From<PipelineError> + Send + 'static>() -> Effect<Tenancy, E, DynEnv> {
  Effect::from_ctx(|ctx: Ctx<DynEnv>| async move {
    ctx
      .env()
      .try_get::<TenantId>()
      .map(|t| Tenancy::new((*t).clone()))
      .map_err(|_| E::from(PipelineError::NoTenant))
  })
}

/// The id the RPC server scopes idempotency keys by: `<tenant>/<principal>`.
pub fn rpc_principal(parts: &axum::http::request::Parts) -> Option<String> {
  parts
    .extensions
    .get::<CurrentPrincipal>()
    .filter(|p| p.is_authenticated())
    .map(|p| match p.tenant() {
      Some(t) => format!("{t}/{}", p.id()),
      None => p.id().to_owned(),
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tenant_ids_and_locales_are_checked() {
    assert!(TenantConfig::new("acme", "en").is_ok());
    assert!(TenantConfig::new("Acme", "en").is_err());
    assert!(TenantConfig::new("", "en").is_err());
    assert!(TenantConfig::new("../x", "en").is_err());
    assert!(TenantConfig::new("acme", "../etc").is_err());
    let globex = TenantConfig::new("globex", "de")
      .unwrap()
      .with_database("postgres://g");
    let tenants = Tenants::new([TenantConfig::new("acme", "en").unwrap(), globex]);
    assert_eq!(tenants.locale("globex").as_str(), "de");
    assert_eq!(tenants.locale("initech").as_str(), "en");
    assert_eq!(
      tenants.database_urls("postgres://main"),
      ["postgres://main", "postgres://g"]
    );
    assert_eq!(tenants.iter().count(), 2);
  }

  #[test]
  fn weak_keys_are_refused_and_never_printed() {
    assert!(TokenKeys::new(vec![0; 8], "iss").is_err());
    assert!(TokenKeys::new(vec![0; 32], " ").is_err());
    let keys = TokenKeys::new(vec![7; 32], "https://id.example").unwrap();
    assert!(format!("{keys:?}").contains("id.example"));
    assert!(!format!("{keys:?}").contains("secret"));
  }
}
