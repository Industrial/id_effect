//! Tenant resolution (ADR 0015 D10): which tenant a request acts for.
//!
//! A [`TenantResolver`] turns a request into a [`TenantId`] from up to three sources:
//!
//! 1. **The principal** (always on): the authenticated principal's tenant claim,
//!    [`Principal::tenant`]. This is the default and, alone, the only source.
//! 2. **The Host** ([`TenantResolver::with_host_allowlist`]): a configured map from host name to
//!    tenant. A host outside the map yields no tenant; the request's host is never used as a
//!    tenant id itself.
//! 3. **A request header** ([`TenantResolver::dangerous_tenant_from_header`]): a value any client
//!    can set, so it is a recorded opt-out (ADR 0013 R10) for deployments where something in
//!    front of the service (a gateway that authenticates and sets the header) vouches for it.
//!
//! Sources that yield a tenant must agree: a principal of tenant `acme` on the host of `globex`
//! resolves to **no tenant** rather than to either of them. No tenant means no tenant-scoped
//! work: `id_effect_sql`'s `CurrentTenant` is absent, tenant-routed connections are refused and
//! the [`TenantId`] extractor (feature `axum`) answers 401/403.
//!
//! A resolved [`TenantId`] is [`Tenanted`], so it carries its own
//! [`TenantScope`](crate::TenantScope): a policy run on it with
//! [`check_with`](crate::check_with) / [`authorize_with`](crate::authorize_with) first requires
//! the principal's tenant to be the one resolved.
//!
//! ```
//! use id_effect_authz::{BasicPrincipal, CurrentPrincipal, TenantId, TenantResolver};
//!
//! let resolver = TenantResolver::new().with_host_allowlist([("acme.example.com", "acme")]);
//! let ada = CurrentPrincipal::new(BasicPrincipal::new("ada").with_tenant("acme"));
//! let visitor = CurrentPrincipal::anonymous();
//!
//! // The principal's claim, by default.
//! assert_eq!(resolver.resolve_with(&ada, None, None), Some(TenantId::new("acme")));
//! // An anonymous visitor on an allowlisted host.
//! assert_eq!(
//!   resolver.resolve_with(&visitor, Some("ACME.example.com:443"), None),
//!   Some(TenantId::new("acme"))
//! );
//! // Any other host yields nothing, and so does a disagreement.
//! assert_eq!(resolver.resolve_with(&visitor, Some("evil.example.com"), None), None);
//! let eve = CurrentPrincipal::new(BasicPrincipal::new("eve").with_tenant("globex"));
//! assert_eq!(resolver.resolve_with(&eve, Some("acme.example.com"), None), None);
//! ```
//!
//! With feature `axum`, `TenantResolver::layer` resolves every request (inside the
//! authentication layer) and records the [`TenantId`] as a request extension and in the
//! request's `id_effect_axum::RequestCaps`, where handlers extract it and `DynEnv` effects find
//! it as a capability.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use id_effect::observability::posture::record_dangerous;
use id_effect::{Effect, HCons, HList, HNil, Selector, hlist, need};

use crate::principal::{CurrentPrincipal, Principal};
use crate::resource::Tenanted;

/// The longest tenant id [`TenantId::parse`] accepts, in bytes.
pub const MAX_TENANT_ID_LEN: usize = 128;

/// The longest Host value considered, in bytes (a DNS name is at most 253; the port and IPv6
/// brackets fit in the rest).
const MAX_HOST_LEN: usize = 300;

/// The tenant a request or job acts for: the capability a [`TenantResolver`] produces.
///
/// It is [`Tenanted`] (so it has a [`TenantScope`](crate::TenantScope)) and cheap to clone.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(Arc<str>);

impl TenantId {
  /// The tenant `id`, taken as it is: for ids from a trusted source (the principal's claim, the
  /// configuration). Use [`TenantId::parse`] for anything a client sent.
  pub fn new(id: impl Into<String>) -> Self {
    Self(Arc::from(id.into()))
  }

  /// A tenant id from untrusted input: `Some` when `raw` is 1 to [`MAX_TENANT_ID_LEN`] bytes of
  /// ASCII letters, digits, `-`, `_` and `.`, otherwise `None`.
  pub fn parse(raw: &str) -> Option<Self> {
    let valid = !raw.is_empty()
      && raw.len() <= MAX_TENANT_ID_LEN
      && raw
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    valid.then(|| Self::new(raw))
  }

  /// The id.
  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for TenantId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("TenantId").field(&&*self.0).finish()
  }
}

impl fmt::Display for TenantId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl AsRef<str> for TenantId {
  fn as_ref(&self) -> &str {
    &self.0
  }
}

impl From<&str> for TenantId {
  fn from(id: &str) -> Self {
    Self::new(id)
  }
}

impl From<String> for TenantId {
  fn from(id: String) -> Self {
    Self::new(id)
  }
}

impl Tenanted for TenantId {
  fn tenant_id(&self) -> &str {
    &self.0
  }
}

/// The minimal typed environment holding only [`TenantId`].
pub type TenantEnv = HCons<TenantId, HNil>;

/// A [`TenantEnv`] holding `tenant` (for `.provide_all(..)` in jobs, tests and entry points).
#[inline]
pub fn tenant_env(tenant: impl Into<TenantId>) -> TenantEnv {
  hlist![tenant.into()]
}

/// The [`TenantId`] in `R`.
#[inline]
pub fn current_tenant<E, R, I>() -> Effect<TenantId, E, R>
where
  E: Send + 'static,
  R: Selector<TenantId, I> + HList,
{
  need::<TenantId>().map(|t| TenantId::clone(&t))
}

/// Resolves the tenant of a request (see the [module docs](self)).
///
/// [`TenantResolver::new`] (also [`Default`]) reads the principal's tenant only. Cheap to clone.
#[derive(Clone, Default)]
pub struct TenantResolver {
  hosts: Option<Arc<HashMap<String, TenantId>>>,
  header: Option<Arc<str>>,
}

impl TenantResolver {
  /// The default resolver: the authenticated principal's tenant claim, nothing else.
  pub fn new() -> Self {
    Self::default()
  }

  /// Also resolve from the request's Host through `hosts`, a map from host name to tenant id.
  ///
  /// Host names compare case-insensitively, without the port and without a trailing dot. A
  /// host outside the map yields no tenant. Calling it again replaces the map.
  #[must_use]
  pub fn with_host_allowlist<H, T>(mut self, hosts: impl IntoIterator<Item = (H, T)>) -> Self
  where
    H: AsRef<str>,
    T: Into<TenantId>,
  {
    let hosts = hosts
      .into_iter()
      .filter_map(|(host, tenant)| Some((normalize_host(host.as_ref())?, tenant.into())))
      .collect();
    self.hosts = Some(Arc::new(hosts));
    self
  }

  /// Also resolve from the request header `name`, whose value must be a valid
  /// [`TenantId::parse`] id.
  ///
  /// A dangerous opt-out (ADR 0013 R1, R10): any client can send any header, so the tenant is
  /// whatever the caller claims unless a gateway in front of the service sets the header and
  /// strips the client's. A tenant from the header must still agree with the principal's claim
  /// and the Host. The call records
  /// `id_effect_authz::TenantResolver::dangerous_tenant_from_header` in the posture registry, so
  /// the prod profile refuses to start until the operator accepts it.
  #[must_use]
  pub fn dangerous_tenant_from_header(mut self, name: impl Into<String>) -> Self {
    record_dangerous(
      "id_effect_authz::TenantResolver::dangerous_tenant_from_header",
      "M19: the tenant is chosen by a request header the client controls",
    );
    self.header = Some(Arc::from(name.into().to_ascii_lowercase()));
    self
  }

  /// The header the tenant is read from, when
  /// [`dangerous_tenant_from_header`](Self::dangerous_tenant_from_header) was taken.
  pub fn tenant_header(&self) -> Option<&str> {
    self.header.as_deref()
  }

  /// Whether a Host allowlist is configured.
  pub fn has_host_allowlist(&self) -> bool {
    self.hosts.is_some()
  }

  /// The tenant of `principal` alone: its claim when it is authenticated, else `None`.
  pub fn resolve_principal(&self, principal: &dyn Principal) -> Option<TenantId> {
    if principal.is_authenticated() {
      principal.tenant().map(TenantId::new)
    } else {
      None
    }
  }

  /// The tenant the Host `host` maps to: `None` without an allowlist or outside it.
  pub fn resolve_host(&self, host: &str) -> Option<TenantId> {
    let hosts = self.hosts.as_ref()?;
    hosts.get(&normalize_host(host)?).cloned()
  }

  /// The tenant for `principal` on a request with the Host `host` and the value `header` of the
  /// configured tenant header (each `None` when absent): every configured source that yields a
  /// tenant must yield the same one; none, or a disagreement, is `None`.
  pub fn resolve_with(
    &self,
    principal: &CurrentPrincipal,
    host: Option<&str>,
    header: Option<&str>,
  ) -> Option<TenantId> {
    let from_principal = self.resolve_principal(&**principal);
    let from_host = host.and_then(|h| self.resolve_host(h));
    let from_header = self
      .header
      .as_ref()
      .and(header)
      .and_then(|value| TenantId::parse(value.trim()));
    let mut found = [from_principal, from_host, from_header]
      .into_iter()
      .flatten();
    let first = found.next()?;
    found.all(|other| other == first).then_some(first)
  }
}

impl fmt::Debug for TenantResolver {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("TenantResolver")
      .field("hosts", &self.hosts.as_ref().map(|h| h.len()))
      .field("header", &self.header)
      .finish()
  }
}

/// `host` lower-cased, without the port and a trailing dot; `None` when empty or too long.
fn normalize_host(host: &str) -> Option<String> {
  let host = host.trim();
  if host.len() > MAX_HOST_LEN {
    return None;
  }
  let name = if let Some(rest) = host.strip_prefix('[') {
    // `[v6]` or `[v6]:port`.
    &host[..rest.find(']')? + 2]
  } else {
    host.rsplit_once(':').map_or(host, |(name, _port)| name)
  };
  let name = name.strip_suffix('.').unwrap_or(name);
  (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

#[cfg(feature = "axum")]
pub use web::{TenantLayer, TenantService};

#[cfg(feature = "axum")]
mod web {
  use std::convert::Infallible;
  use std::fmt;
  use std::future::Future;
  use std::pin::Pin;
  use std::task::{Context, Poll};

  use axum::extract::{FromRequestParts, OptionalFromRequestParts};
  use axum::http::request::Parts;
  use axum::http::{Request, header};
  use axum::response::{IntoResponse, Response};
  use id_effect_axum::RequestCaps;
  use tower_layer::Layer;
  use tower_service::Service;

  use super::{TenantId, TenantResolver};
  use crate::guard::principal_of;
  use crate::policy::{AuthzError, Denial};

  /// The denial reason when a request has no tenant.
  const NO_TENANT: &str = "no tenant";

  impl TenantResolver {
    /// The tenant of the request head `parts`: [`resolve_with`](Self::resolve_with) over the
    /// request's principal ([`principal_of`]), its Host (the `Host` header, else the URI's
    /// authority) and the configured tenant header.
    pub fn resolve(&self, parts: &Parts) -> Option<TenantId> {
      let principal = principal_of(parts);
      let host = parts
        .headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| parts.uri.authority().map(|a| a.as_str()));
      let value = self
        .tenant_header()
        .and_then(|name| parts.headers.get(name))
        .and_then(|v| v.to_str().ok());
      self.resolve_with(&principal, host, value)
    }

    /// A Tower layer resolving every request's tenant with this resolver (see
    /// [`TenantLayer`]).
    pub fn layer(&self) -> TenantLayer {
      TenantLayer {
        resolver: self.clone(),
        required: false,
      }
    }
  }

  /// Why a request has no tenant: 401 for an anonymous principal, 403 otherwise.
  fn missing(parts: &Parts) -> AuthzError {
    if principal_of(parts).is_authenticated() {
      AuthzError::Denied(Denial::new(NO_TENANT))
    } else {
      AuthzError::Unauthenticated
    }
  }

  /// The request's [`TenantId`]: a request extension, else a [`RequestCaps`] override.
  fn tenant_of(parts: &Parts) -> Option<TenantId> {
    parts.extensions.get::<TenantId>().cloned().or_else(|| {
      parts
        .extensions
        .get::<RequestCaps>()
        .and_then(RequestCaps::get::<TenantId>)
        .map(|t| TenantId::clone(&t))
    })
  }

  /// A Tower layer that resolves each request's tenant with its [`TenantResolver`] and records
  /// the [`TenantId`] as a request extension and in the request's [`RequestCaps`] (so a
  /// `DynEnv` effect run with `id_effect_axum::execute_with_request_caps` has it as a
  /// capability). Built with [`TenantResolver::layer`].
  ///
  /// It reads the principal, so it goes **inside** the authentication layer (added to the
  /// router before it). A request without a tenant passes on without one, unless
  /// [`require_tenant`](Self::require_tenant) is set.
  #[derive(Clone)]
  pub struct TenantLayer {
    resolver: TenantResolver,
    required: bool,
  }

  impl TenantLayer {
    /// Answer a request without a tenant here (401 for an anonymous principal, 403 otherwise,
    /// as Problem Details) instead of passing it on.
    #[must_use]
    pub fn require_tenant(mut self) -> Self {
      self.required = true;
      self
    }
  }

  impl fmt::Debug for TenantLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("TenantLayer")
        .field("resolver", &self.resolver)
        .field("required", &self.required)
        .finish()
    }
  }

  impl<S> Layer<S> for TenantLayer {
    type Service = TenantService<S>;

    fn layer(&self, inner: S) -> Self::Service {
      TenantService {
        layer: self.clone(),
        inner,
      }
    }
  }

  /// The service a [`TenantLayer`] wraps a route in.
  #[derive(Clone, Debug)]
  pub struct TenantService<S> {
    layer: TenantLayer,
    inner: S,
  }

  impl<S, B> Service<Request<B>> for TenantService<S>
  where
    S: Service<Request<B>, Response = Response, Error = Infallible>,
    S::Future: Send + 'static,
  {
    type Response = Response;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
      self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
      let (mut parts, body) = req.into_parts();
      match self.layer.resolver.resolve(&parts) {
        Some(tenant) => {
          let mut caps = parts.extensions.remove::<RequestCaps>().unwrap_or_default();
          caps.insert(tenant.clone());
          parts.extensions.insert(caps);
          parts.extensions.insert(tenant);
        }
        None if self.layer.required => {
          let response = missing(&parts).into_response();
          return Box::pin(std::future::ready(Ok(response)));
        }
        None => {}
      }
      Box::pin(self.inner.call(Request::from_parts(parts, body)))
    }
  }

  /// The request's tenant, as resolved by a [`TenantLayer`] (or recorded in [`RequestCaps`]).
  /// Without one the handler is not called: 401 for an anonymous principal, 403 otherwise.
  impl<S: Send + Sync> FromRequestParts<S> for TenantId {
    type Rejection = AuthzError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
      tenant_of(parts).ok_or_else(|| missing(parts))
    }
  }

  /// `Option<TenantId>` in a handler: the request's tenant, if any.
  impl<S: Send + Sync> OptionalFromRequestParts<S> for TenantId {
    type Rejection = Infallible;

    async fn from_request_parts(
      parts: &mut Parts,
      _state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
      Ok(tenant_of(parts))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::principal::BasicPrincipal;
  use id_effect::Exit;

  #[test]
  fn tenant_id_parses_bounded_ids_only() {
    assert_eq!(
      TenantId::parse("acme-1_eu.prod"),
      Some(TenantId::new("acme-1_eu.prod"))
    );
    assert_eq!(TenantId::parse(""), None);
    assert_eq!(TenantId::parse("acme corp"), None);
    assert_eq!(TenantId::parse("acme/../globex"), None);
    assert_eq!(
      TenantId::parse(&"a".repeat(MAX_TENANT_ID_LEN)).map(|t| t.as_str().len()),
      Some(128)
    );
    assert_eq!(TenantId::parse(&"a".repeat(MAX_TENANT_ID_LEN + 1)), None);
    let t = TenantId::from(String::from("acme"));
    assert_eq!(
      (t.to_string(), format!("{t:?}")),
      ("acme".into(), "TenantId(\"acme\")".into())
    );
    assert_eq!(t.as_ref(), "acme");
    assert_eq!(t.tenant_id(), "acme");
  }

  #[test]
  fn hosts_normalize() {
    assert_eq!(
      normalize_host(" Acme.Example.COM.:8443 ").as_deref(),
      Some("acme.example.com")
    );
    assert_eq!(normalize_host("[::1]:8080").as_deref(), Some("[::1]"));
    assert_eq!(normalize_host("[::1]").as_deref(), Some("[::1]"));
    assert_eq!(normalize_host("[::1"), None);
    assert_eq!(normalize_host(""), None);
    assert_eq!(normalize_host(":80"), None);
    assert_eq!(normalize_host(&"a".repeat(MAX_HOST_LEN + 1)), None);
  }

  #[test]
  fn resolver_sources_and_env() {
    let plain = TenantResolver::new();
    assert!(!plain.has_host_allowlist() && plain.tenant_header().is_none());
    assert_eq!(plain.resolve_host("acme.example.com"), None);
    let anon = CurrentPrincipal::anonymous();
    assert_eq!(
      plain.resolve_with(&anon, Some("acme.example.com"), Some("acme")),
      None
    );
    let with_hosts = plain.with_host_allowlist([("", "ignored"), ("acme.example.com", "acme")]);
    assert!(with_hosts.has_host_allowlist());
    assert!(format!("{with_hosts:?}").contains("hosts: Some(1)"));
    let p = CurrentPrincipal::new(BasicPrincipal::new("ada").with_tenant("acme"));
    assert_eq!(
      with_hosts.resolve_with(&p, Some("acme.example.com"), None),
      Some(TenantId::new("acme"))
    );
    let tenant = current_tenant::<(), _, _>().provide_all(tenant_env("acme"));
    assert_eq!(tenant.block_on(), Exit::Success(TenantId::new("acme")));
  }
}
