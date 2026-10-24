//! One tenant for authorization and queries (ADR 0015 D10); feature `tenancy`.
//!
//! `id_effect_authz` resolves the tenant a request acts for ([`TenantResolver`]: the
//! authenticated principal's claim by default, a configured Host allowlist, and a request header
//! only through the recorded `dangerous_tenant_from_header`). `id_effect_sql` filters
//! tenant-scoped entities by a [`CurrentTenant`] and routes connections by tenant
//! ([`TenantRouter`]). The two crates do not depend on each other; this module bridges them, so
//! authorization and queries can never disagree about the tenant:
//!
//! - [`Tenancy`] is one resolved tenant seen both ways: the [`TenantId`] authorization checks
//!   run against (it is a [`TenantScope`]: a policy run on it first requires the principal's
//!   tenant to be this one) and the [`CurrentTenant`] repositories filter by.
//! - With feature `web`, `TenancyLayer` resolves every request once and records both in the
//!   request's extensions and `RequestCaps` (so `DynEnv` effects find either as a capability),
//!   and a handler takes `Tenancy` as an extractor.
//!
//! No tenant (an anonymous principal, a Host outside the allowlist, sources that disagree) means
//! neither: no `TenantId`, no `CurrentTenant`, and a tenant-scoped repository refuses to run.
//!
//! ```rust
//! use id_effect_kit::authz::{BasicPrincipal, CurrentPrincipal};
//! use id_effect_kit::sql::{CurrentTenant, SqlValue};
//! use id_effect_kit::tenancy::{Tenancy, TenantId, TenantResolver};
//!
//! let resolver = TenantResolver::new();
//! let ada = CurrentPrincipal::new(BasicPrincipal::new("ada").with_tenant("acme"));
//! let tenancy = Tenancy::resolve(&resolver, &ada, None, None).expect("ada's tenant claim");
//! assert_eq!(tenancy.tenant(), &TenantId::new("acme"));
//! assert_eq!(tenancy.sql_tenant(), &CurrentTenant(SqlValue::Text("acme".into())));
//!
//! // An anonymous visitor has no tenant: neither side is set.
//! assert!(Tenancy::resolve(&resolver, &CurrentPrincipal::anonymous(), None, None).is_none());
//! ```

use id_effect::{Effect, HCons, HList, HNil, Selector, hlist};
use id_effect_authz::{AuthzError, CurrentPrincipal, Decision, Policy, check_with};
use id_effect_sql::{Dialect, Entity, Repository};

pub use id_effect_authz::{
  TenantEnv, TenantId, TenantResolver, TenantScope, Tenanted, current_tenant, tenant_env,
};
pub use id_effect_sql::{CurrentTenant, TenantRouter};

#[cfg(feature = "web")]
pub use web::{SqlTenantService, TenancyLayer};

/// The environment holding both sides of one tenant: the [`TenantId`] and the
/// [`CurrentTenant`] ([`Tenancy::env`]).
pub type TenancyEnv = HCons<TenantId, HCons<CurrentTenant, HNil>>;

/// The [`CurrentTenant`] of `tenant`: its id as SQL text, the value tenant-scoped entities are
/// filtered by and written with.
pub fn sql_tenant(tenant: &TenantId) -> CurrentTenant {
  CurrentTenant::new(tenant.as_str().to_owned())
}

/// One resolved tenant, for authorization ([`TenantId`], a [`TenantScope`]) and for queries
/// ([`CurrentTenant`], [`TenantRouter`]) alike. See the [module docs](self).
#[derive(Clone, Debug, PartialEq)]
pub struct Tenancy {
  tenant: TenantId,
  sql: CurrentTenant,
}

impl Tenancy {
  /// Both sides of `tenant` (an id from a trusted source: a resolver, the configuration, a job's
  /// payload written by the app).
  pub fn new(tenant: impl Into<TenantId>) -> Self {
    let tenant = tenant.into();
    Self {
      sql: sql_tenant(&tenant),
      tenant,
    }
  }

  /// The tenant `resolver` resolves for `principal` on a request with the Host `host` and the
  /// configured tenant header's value `header` ([`TenantResolver::resolve_with`]); `None` when
  /// there is none (and then neither side is set).
  pub fn resolve(
    resolver: &TenantResolver,
    principal: &CurrentPrincipal,
    host: Option<&str>,
    header: Option<&str>,
  ) -> Option<Self> {
    resolver
      .resolve_with(principal, host, header)
      .map(Self::new)
  }

  /// The authorization side: the tenant id policies are scoped by.
  pub fn tenant(&self) -> &TenantId {
    &self.tenant
  }

  /// The SQL side: the value tenant-scoped repositories filter by.
  pub fn sql_tenant(&self) -> &CurrentTenant {
    &self.sql
  }

  /// Both sides as an environment, for `.provide_all(..)` in jobs, tests and entry points:
  /// `need::<TenantId>()`, [`current_tenant`] and `Repository::for_current_tenant` read it.
  pub fn env(&self) -> TenancyEnv {
    hlist![self.tenant.clone(), self.sql.clone()]
  }

  /// A repository for `dialect` scoped to this tenant (its statements filter by the tenant
  /// column, its inserts write it).
  pub fn repository<T: Entity>(&self, dialect: Dialect) -> Repository<T> {
    Repository::new(dialect).with_tenant(self.sql.0.clone())
  }

  /// Runs `effect` acting for this tenant: statements it sends through a [`TenantRouter`] go to
  /// this tenant's database ([`TenantRouter::with_tenant`]).
  pub fn scope<A, E, R>(&self, effect: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    TenantRouter::with_tenant(self.tenant.as_str(), effect)
  }

  /// `policy` for the [`CurrentPrincipal`] in `R`, on this tenant: behind the tenant check, so a
  /// principal of another tenant is denied whatever `policy` says ([`check_with`]).
  pub fn check<P, R, I>(&self, policy: P) -> Effect<Decision, AuthzError, R>
  where
    P: Policy<TenantId>,
    R: Selector<CurrentPrincipal, I> + HList,
  {
    check_with(policy, self.tenant.clone())
  }
}

impl From<TenantId> for Tenancy {
  fn from(tenant: TenantId) -> Self {
    Self::new(tenant)
  }
}

#[cfg(feature = "web")]
mod web {
  use std::convert::Infallible;
  use std::fmt;
  use std::task::{Context, Poll};

  use axum::extract::{FromRequestParts, OptionalFromRequestParts};
  use axum::http::Request;
  use axum::http::request::Parts;
  use id_effect_authz::{AuthzError, TenantLayer, TenantResolver, TenantService};
  use id_effect_axum::RequestCaps;
  use tower_layer::Layer;
  use tower_service::Service;

  use super::{Tenancy, TenantId, sql_tenant};

  /// A Tower layer that resolves each request's tenant once, with one [`TenantResolver`], and
  /// records both sides: the [`TenantId`] (`id_effect_authz::TenantLayer`) and its
  /// [`CurrentTenant`](super::CurrentTenant), each as a request extension and in the request's
  /// [`RequestCaps`].
  ///
  /// It reads the principal, so it goes **inside** the authentication layer (added to the router
  /// before it). A request without a tenant passes on with neither, unless
  /// [`require_tenant`](Self::require_tenant) is set.
  #[derive(Clone)]
  pub struct TenancyLayer {
    tenant: TenantLayer,
  }

  impl TenancyLayer {
    /// The layer resolving with `resolver`.
    pub fn new(resolver: &TenantResolver) -> Self {
      Self {
        tenant: resolver.layer(),
      }
    }

    /// Answer a request without a tenant here (401 for an anonymous principal, 403 otherwise,
    /// as Problem Details) instead of passing it on.
    #[must_use]
    pub fn require_tenant(self) -> Self {
      Self {
        tenant: self.tenant.require_tenant(),
      }
    }
  }

  impl fmt::Debug for TenancyLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("TenancyLayer")
        .field("tenant", &self.tenant)
        .finish()
    }
  }

  impl<S> Layer<S> for TenancyLayer {
    type Service = TenantService<SqlTenantService<S>>;

    fn layer(&self, inner: S) -> Self::Service {
      self.tenant.layer(SqlTenantService { inner })
    }
  }

  /// The inner half of a [`TenancyLayer`]: records the [`CurrentTenant`](super::CurrentTenant)
  /// of the request's resolved [`TenantId`].
  #[derive(Clone, Debug)]
  pub struct SqlTenantService<S> {
    inner: S,
  }

  impl<S, B> Service<Request<B>> for SqlTenantService<S>
  where
    S: Service<Request<B>>,
  {
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
      self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
      if let Some(tenant) = req.extensions().get::<TenantId>().cloned() {
        let sql = sql_tenant(&tenant);
        let extensions = req.extensions_mut();
        let mut caps = extensions.remove::<RequestCaps>().unwrap_or_default();
        caps.insert(sql.clone());
        extensions.insert(caps);
        extensions.insert(sql);
      }
      self.inner.call(req)
    }
  }

  /// The request's tenant, both sides, as a [`TenancyLayer`] (or `id_effect_authz`'s
  /// `TenantLayer`) resolved it. Without one the handler is not called: 401 for an anonymous
  /// principal, 403 otherwise.
  impl<S: Send + Sync> FromRequestParts<S> for Tenancy {
    type Rejection = AuthzError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
      <TenantId as FromRequestParts<S>>::from_request_parts(parts, state)
        .await
        .map(Tenancy::new)
    }
  }

  /// `Option<Tenancy>` in a handler: the request's tenant, if any.
  impl<S: Send + Sync> OptionalFromRequestParts<S> for Tenancy {
    type Rejection = Infallible;

    async fn from_request_parts(
      parts: &mut Parts,
      state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
      <TenantId as OptionalFromRequestParts<S>>::from_request_parts(parts, state)
        .await
        .map(|tenant| tenant.map(Tenancy::new))
    }
  }
}
