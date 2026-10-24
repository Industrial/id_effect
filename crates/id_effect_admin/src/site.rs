//! [`AdminSite`]: the registration API (web.admin #8) and the guarded routes it mounts.
//!
//! An admin site is built in three steps: register entities, decide who may enter, mount.
//!
//! 1. [`AdminSite::register`] adds an entity with its [`EntityAdmin`] configuration, checked
//!    against the entity's field metadata.
//! 2. [`AdminSite::guard`] attaches a route guard (an `id_effect_authz::Guard`, or any
//!    [`RouteGuard`]) to every admin route. The site is [`Unguarded`] until then and only a
//!    [`Guarded`] site has [`AdminSite::router`]: there is no public admin, and forgetting the guard
//!    is a compile error (ADR 0013 R1, R7; the H8 typestate of `id_effect_axum` routes).
//! 3. [`AdminSite::router`] returns an `id_effect_axum` typed [`Router`] whose routes are all
//!    registered as guarded, to merge into the application's router.
//!
//! Inside the guard, every page checks the entity × action permission
//! ([`AdminPermission`](crate::AdminPermission)) through the site's `id_effect_authz` policy
//! before any query, and reads rows through a repository bound to the request's
//! `CurrentTenant` (a tenant-scoped entity without a tenant is refused, never read unscoped).
//!
//! | Route | Page | Permission |
//! |---|---|---|
//! | `GET <prefix>` | the entities the principal may list | `admin.<entity>.list` per entry |
//! | `GET <prefix>/<entity>` | list: search, filters, sort, keyset pages | `admin.<entity>.list` |
//! | `GET <prefix>/<entity>/{id}` | detail | `admin.<entity>.view` |
//! | `GET`, `POST <prefix>/<entity>/@new` | create ([`edit`](crate::edit)) | `admin.<entity>.create` |
//! | `GET`, `POST <prefix>/<entity>/{id}/@edit` | update | `admin.<entity>.update` |
//! | `GET`, `POST <prefix>/<entity>/{id}/@delete` | delete, after a confirmation | `admin.<entity>.delete` |
//! | `POST <prefix>/<entity>/@bulk` | a bulk action ([`bulk`](crate::bulk)) | `admin.<entity>.bulk` and the action's own |
//! | `GET <prefix>/<entity>/@pick/<relation>` | picker options ([`relations`](crate::relations)) | `create` or `update`, `list` on the target |
//! | `POST <prefix>/<entity>/{id}/@rel/<relation>` | add / remove a join-table pair | `update`, `list` on the target |
//!
//! Every `POST` also needs the CSRF check of `id_effect_axum`'s `SecurityLayer` (install it
//! around the application; a write it did not check is refused with 403) and reads at most
//! 64 KiB of form ([`forms`](crate::forms)).
//!
//! A site without a guard has no router:
//!
//! ```compile_fail
//! use id_effect_admin::{AdminDatabase, AdminSite};
//! use id_effect_sql::{Dialect, TestSqlClient};
//!
//! let db = AdminDatabase::new(std::sync::Arc::new(TestSqlClient::new()), Dialect::Sqlite);
//! let _ = AdminSite::new().router::<()>(db);
//! ```

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::{Path, Request};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use axum::routing::MethodRouter;
use id_effect_authz::guard::principal_of;
use id_effect_authz::{AuthzError, BoxPolicy, CurrentPrincipal, Policy, Rbac};
use id_effect_axum::RequestCaps;
use id_effect_axum::route::{Endpoint, GuardFuture, RouteGuard, RouteInfo, Router};
use id_effect_axum::ui::{BasicLayout, Layout, LayoutContext, Markup, Page, html, respond};
use id_effect_sql::{CurrentTenant, Dialect, Entity, Repository, SqlClient, SqlClientService};

use crate::edit::{AdminRoute, Typed};
use crate::entity::{AdminConfigError, EntityAdmin, EntityInfo, EntitySpec};
use crate::labels::Labels;
use crate::permission::{AdminAction, AdminPermission, RbacAdminPolicy, authorize};
use crate::relations::{Lookup, Reader};
use crate::value::{encode, humanize};

/// The default path prefix of an admin site.
pub const DEFAULT_PREFIX: &str = "/admin";

mod sealed {
  pub trait Sealed {}
}

/// The access state of an [`AdminSite`]: [`Unguarded`] or [`Guarded`]. Sealed.
pub trait SiteAccess: sealed::Sealed + Send + Sync + 'static {}

/// No guard yet: the site cannot be mounted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unguarded;

/// At least one guard decides before every admin route ([`AdminSite::guard`]).
#[derive(Clone)]
pub struct Guarded {
  first: Arc<dyn RouteGuard>,
  more: Vec<Arc<dyn RouteGuard>>,
}

impl fmt::Debug for Guarded {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_list()
      .entries(self.policies().collect::<Vec<_>>())
      .finish()
  }
}

impl Guarded {
  /// The guards' policy names, outermost last.
  pub fn policies(&self) -> impl Iterator<Item = &str> {
    std::iter::once(&self.first)
      .chain(&self.more)
      .map(|guard| guard.policy())
  }
}

impl sealed::Sealed for Unguarded {}
impl sealed::Sealed for Guarded {}
impl SiteAccess for Unguarded {}
impl SiteAccess for Guarded {}

/// One shared guard, attached to every admin endpoint.
struct SharedGuard(Arc<dyn RouteGuard>);

impl RouteGuard for SharedGuard {
  fn policy(&self) -> &str {
    self.0.policy()
  }

  fn check<'a>(&'a self, parts: &'a Parts) -> GuardFuture<'a> {
    self.0.check(parts)
  }
}

/// The database the admin reads: a client and the dialect its statements are rendered for.
#[derive(Clone)]
pub struct AdminDatabase {
  client: SqlClientService,
  dialect: Dialect,
}

impl AdminDatabase {
  /// Read through `client` (any `SqlClient`: a pool, a replica router, a tenant router) in
  /// `dialect`.
  pub fn new(client: Arc<dyn SqlClient>, dialect: Dialect) -> Self {
    Self {
      client: SqlClientService(client),
      dialect,
    }
  }

  /// The dialect.
  pub fn dialect(&self) -> Dialect {
    self.dialect
  }

  pub(crate) fn client(&self) -> SqlClientService {
    self.client.clone()
  }
}

impl fmt::Debug for AdminDatabase {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AdminDatabase")
      .field("dialect", &self.dialect)
      .finish_non_exhaustive()
  }
}

/// A registered entity: its checked configuration and its routes.
trait Mount: Send + Sync {
  fn spec(&self) -> &Arc<EntitySpec>;
  fn list(&self, site: Arc<SiteShared>) -> MethodRouter;
  fn detail(&self, site: Arc<SiteShared>) -> MethodRouter;
  /// The create, edit, delete, bulk and relation routes.
  fn edit_routes(&self, site: &Arc<SiteShared>) -> Vec<AdminRoute>;
  /// The entity as the target of other entities' relations.
  fn lookup(&self) -> Arc<dyn Lookup>;
}

struct Registered<E> {
  spec: Arc<EntitySpec>,
  typed: Arc<Typed<E>>,
  entity: PhantomData<fn() -> E>,
}

impl<E: Entity> Mount for Registered<E> {
  fn spec(&self) -> &Arc<EntitySpec> {
    &self.spec
  }

  fn list(&self, site: Arc<SiteShared>) -> MethodRouter {
    let spec = Arc::clone(&self.spec);
    axum::routing::get(move |req: Request| {
      crate::list::list::<E>(Arc::clone(&site), Arc::clone(&spec), req)
    })
  }

  fn detail(&self, site: Arc<SiteShared>) -> MethodRouter {
    let spec = Arc::clone(&self.spec);
    axum::routing::get(move |Path(id): Path<String>, req: Request| {
      crate::detail::detail::<E>(Arc::clone(&site), Arc::clone(&spec), id, req)
    })
  }

  fn edit_routes(&self, site: &Arc<SiteShared>) -> Vec<AdminRoute> {
    let mut routes = crate::edit::routes::<E>(site, &self.spec, &self.typed);
    routes.push(crate::bulk::route::<E>(site, &self.spec, &self.typed));
    routes.extend(crate::relations::routes::<E>(site, &self.spec));
    routes
  }

  fn lookup(&self) -> Arc<dyn Lookup> {
    Arc::new(Reader::<E>::new(Arc::clone(&self.spec)))
  }
}

/// An admin site; see the [module docs](self).
///
/// `A` is the access state: [`Unguarded`] until [`guard`](AdminSite::guard) is called, then
/// [`Guarded`]; only a guarded site has a [`router`](AdminSite::router).
///
/// ```
/// use std::sync::Arc;
///
/// use id_effect_admin::{AdminDatabase, AdminSite, EntityAdmin};
/// use id_effect_authz::Guard;
/// use id_effect_authz::require_role;
/// use id_effect_sql::{Dialect, Entity, FromRow, TestSqlClient};
///
/// #[derive(Clone, Debug, FromRow, Entity)]
/// #[entity(table = "documents")]
/// struct Document {
///   #[entity(id)]
///   id: i64,
///   title: String,
/// }
///
/// let router = AdminSite::new()
///   .register::<Document>(EntityAdmin::default().search(["title"]))
///   .guard(Guard::new("staff", require_role("staff")))
///   .router::<()>(AdminDatabase::new(Arc::new(TestSqlClient::new()), Dialect::Sqlite));
/// let report = router.access_report();
/// assert!(report.public.is_empty());
/// // index, list, detail; create, edit, delete (GET and POST each); bulk; picker, relations
/// assert_eq!(report.guarded.len(), 12);
/// ```
pub struct AdminSite<A: SiteAccess = Unguarded> {
  prefix: String,
  title: Option<String>,
  entities: Vec<Arc<dyn Mount>>,
  policy: BoxPolicy<AdminPermission>,
  layout: Option<Arc<dyn Layout>>,
  stylesheets: Vec<String>,
  #[cfg(feature = "i18n")]
  catalog: Option<id_effect_i18n::Catalog>,
  /// Where changes are audited (feature `audit`; see [`audit`](crate::audit)).
  #[cfg(feature = "audit")]
  pub(crate) audit: Option<id_effect_sql::audit::AuditLog>,
  access: A,
}

impl<A: SiteAccess + fmt::Debug> fmt::Debug for AdminSite<A> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AdminSite")
      .field("prefix", &self.prefix)
      .field(
        "entities",
        &self
          .entities
          .iter()
          .map(|e| e.spec().slug.as_str())
          .collect::<Vec<_>>(),
      )
      .field("access", &self.access)
      .finish_non_exhaustive()
  }
}

impl Default for AdminSite<Unguarded> {
  fn default() -> Self {
    Self::new()
  }
}

impl AdminSite<Unguarded> {
  /// An empty, unguarded site at [`DEFAULT_PREFIX`], with the default-deny
  /// [`RbacAdminPolicy`] (only principals' own permissions count).
  pub fn new() -> Self {
    Self {
      prefix: DEFAULT_PREFIX.to_owned(),
      title: None,
      entities: Vec::new(),
      policy: Arc::new(RbacAdminPolicy::default()),
      layout: None,
      stylesheets: Vec::new(),
      #[cfg(feature = "i18n")]
      catalog: None,
      #[cfg(feature = "audit")]
      audit: Some(id_effect_sql::audit::AuditLog::new()),
      access: Unguarded,
    }
  }

  /// Every admin route checks `guard` first (an `id_effect_authz::Guard` answers 401 to the
  /// anonymous principal and 403 to a principal its policy denies).
  pub fn guard(self, guard: impl RouteGuard) -> AdminSite<Guarded> {
    let first: Arc<dyn RouteGuard> = Arc::new(guard);
    self.with_access(Guarded {
      first,
      more: Vec::new(),
    })
  }
}

impl AdminSite<Guarded> {
  /// Also check `guard` (outside the guards added before).
  pub fn guard(mut self, guard: impl RouteGuard) -> Self {
    self.access.more.push(Arc::new(guard));
    self
  }

  /// The guards, in the order they run last to first.
  pub fn guards(&self) -> &Guarded {
    &self.access
  }

  /// The admin's routes, reading `db`: a typed router whose every route is guarded by this
  /// site's guards (merge it into the application's `Router` with `Router::merge`). The
  /// application serves the htmx script the pages load
  /// (`id_effect_axum::ui::htmx::asset_route`).
  pub fn router<S: Clone + Send + Sync + 'static>(self, db: AdminDatabase) -> Router<S> {
    let site = Arc::new(SiteShared {
      prefix: self.prefix,
      title: self.title,
      db,
      policy: self.policy,
      layout: self.layout,
      stylesheets: self.stylesheets,
      entities: self.entities.iter().map(|e| Arc::clone(e.spec())).collect(),
      lookups: self.entities.iter().map(|e| e.lookup()).collect(),
      #[cfg(feature = "i18n")]
      catalog: self.catalog,
      #[cfg(feature = "audit")]
      audit: self.audit,
    });
    let guards = self.access;
    let endpoint =
      |method: Method, path: String, handler: &'static str, method_router: MethodRouter| {
        let mut info = RouteInfo::new(method, path);
        info.handler = Some(handler);
        let first = SharedGuard(Arc::clone(&guards.first));
        let endpoint =
          Endpoint::<S, _>::from_parts(info, method_router.with_state(())).guard(first);
        guards
          .more
          .iter()
          .fold(endpoint, |e, g| e.guard(SharedGuard(Arc::clone(g))))
      };
    let index_site = Arc::clone(&site);
    let mut router = Router::new().route(endpoint(
      Method::GET,
      site.prefix.clone(),
      "id_effect_admin::index",
      axum::routing::get(move |req: Request| index(Arc::clone(&index_site), req)),
    ));
    for entity in &self.entities {
      let slug = &entity.spec().slug;
      router = router
        .route(endpoint(
          Method::GET,
          site.list_path(slug),
          "id_effect_admin::list",
          entity.list(Arc::clone(&site)),
        ))
        .route(endpoint(
          Method::GET,
          format!("{}/{{id}}", site.list_path(slug)),
          "id_effect_admin::detail",
          entity.detail(Arc::clone(&site)),
        ));
      for route in entity.edit_routes(&site) {
        router = router.route(endpoint(
          route.method,
          route.path,
          route.handler,
          route.router,
        ));
      }
    }
    router
  }
}

impl<A: SiteAccess> AdminSite<A> {
  fn with_access<B: SiteAccess>(self, access: B) -> AdminSite<B> {
    AdminSite {
      prefix: self.prefix,
      title: self.title,
      entities: self.entities,
      policy: self.policy,
      layout: self.layout,
      stylesheets: self.stylesheets,
      #[cfg(feature = "i18n")]
      catalog: self.catalog,
      #[cfg(feature = "audit")]
      audit: self.audit,
      access,
    }
  }

  /// Mount the site under `prefix` instead of [`DEFAULT_PREFIX`]: `/` followed by one or more
  /// segments of `a-z`, `0-9`, `-` and `_` (`/admin`, `/back-office/admin`).
  ///
  /// # Panics
  ///
  /// When `prefix` is not of that shape ([`AdminConfigError::InvalidPrefix`]).
  #[must_use]
  pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
    let prefix = prefix.into();
    let valid = prefix.strip_prefix('/').is_some_and(|rest| {
      rest.split('/').all(|segment| {
        !segment.is_empty()
          && segment
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
      })
    });
    if !valid {
      panic!("{}", AdminConfigError::InvalidPrefix { prefix });
    }
    self.prefix = prefix;
    self
  }

  /// The path prefix.
  pub fn path_prefix(&self) -> &str {
    &self.prefix
  }

  /// The site name in page titles (default: the `admin` label, "Administration").
  #[must_use]
  pub fn title(mut self, title: impl Into<String>) -> Self {
    self.title = Some(title.into());
    self
  }

  /// Register the entity `E`, shown as `admin` configures.
  ///
  /// # Panics
  ///
  /// When the configuration is refused ([`try_register`](Self::try_register) returns the
  /// error instead).
  #[must_use]
  pub fn register<E: Entity>(self, admin: EntityAdmin<E>) -> Self {
    match self.try_register(admin) {
      Ok(site) => site,
      Err(error) => panic!("{error}"),
    }
  }

  /// Register the entity `E`, or the [`AdminConfigError`] that refuses the configuration
  /// (including a slug another registered entity already uses).
  ///
  /// # Errors
  ///
  /// See [`AdminConfigError`].
  pub fn try_register<E: Entity>(
    mut self,
    admin: EntityAdmin<E>,
  ) -> Result<Self, AdminConfigError> {
    let spec = admin.spec()?;
    if self.entities.iter().any(|e| e.spec().slug == spec.slug) {
      return Err(AdminConfigError::DuplicateSlug { slug: spec.slug });
    }
    self.entities.push(Arc::new(Registered::<E> {
      spec: Arc::new(spec),
      typed: Arc::new(Typed::new(&admin.edit)),
      entity: PhantomData,
    }));
    Ok(self)
  }

  /// Decide every entity × action permission with `policy` instead of the default
  /// [`RbacAdminPolicy`].
  #[must_use]
  pub fn policy(mut self, policy: impl Policy<AdminPermission>) -> Self {
    self.policy = Arc::new(policy);
    self
  }

  /// Grant permissions through `rbac` ([`RbacAdminPolicy::new`]).
  #[must_use]
  pub fn rbac(self, rbac: Rbac) -> Self {
    self.policy(RbacAdminPolicy::new(rbac))
  }

  /// Wrap pages in `layout` instead of a [`BasicLayout`] titled with the site name.
  #[must_use]
  pub fn layout(mut self, layout: impl Layout) -> Self {
    self.layout = Some(Arc::new(layout));
    self
  }

  /// Link a stylesheet from the default layout (same origin under the default CSP).
  #[must_use]
  pub fn stylesheet(mut self, href: impl Into<String>) -> Self {
    self.stylesheets.push(href.into());
    self
  }

  /// Translate labels and page chrome with `catalog` in the request's locale (see
  /// [`labels`](crate::labels)).
  #[cfg(feature = "i18n")]
  #[must_use]
  pub fn catalog(mut self, catalog: id_effect_i18n::Catalog) -> Self {
    self.catalog = Some(catalog);
    self
  }

  /// The registered entities, in registration order.
  pub fn entities(&self) -> Vec<EntityInfo> {
    self.entities.iter().map(|e| e.spec().info()).collect()
  }
}

/// What every admin handler shares.
pub(crate) struct SiteShared {
  pub(crate) prefix: String,
  title: Option<String>,
  pub(crate) db: AdminDatabase,
  policy: BoxPolicy<AdminPermission>,
  layout: Option<Arc<dyn Layout>>,
  stylesheets: Vec<String>,
  entities: Vec<Arc<EntitySpec>>,
  /// Every registered entity as a relation target.
  lookups: Vec<Arc<dyn Lookup>>,
  #[cfg(feature = "i18n")]
  pub(crate) catalog: Option<id_effect_i18n::Catalog>,
  #[cfg(feature = "audit")]
  pub(crate) audit: Option<id_effect_sql::audit::AuditLog>,
}

impl SiteShared {
  pub(crate) fn list_path(&self, slug: &str) -> String {
    format!("{}/{slug}", self.prefix)
  }

  pub(crate) fn detail_path(&self, slug: &str, id: &str) -> String {
    format!("{}/{slug}/{}", self.prefix, encode(id))
  }

  /// The registered entity named `entity` (`Entity::NAME`), as a relation target.
  pub(crate) fn lookup(&self, entity: &str) -> Option<&Arc<dyn Lookup>> {
    self.lookups.iter().find(|l| l.entity() == entity)
  }
}

/// One admin request: its head, principal, labels and layout context.
pub(crate) struct Ctx {
  pub(crate) parts: Parts,
  principal: CurrentPrincipal,
  pub(crate) labels: Labels,
  pub(crate) layout: LayoutContext,
}

impl Ctx {
  pub(crate) fn new(site: &SiteShared, parts: Parts) -> Self {
    Self {
      principal: principal_of(&parts),
      labels: Labels::for_request(site, &parts),
      layout: LayoutContext::from_parts(&parts.headers, &parts.extensions),
      parts,
    }
  }

  /// Whether the principal holds `action` on `spec`.
  pub(crate) async fn allowed(
    &self,
    site: &SiteShared,
    spec: &EntitySpec,
    action: AdminAction,
  ) -> Result<(), AuthzError> {
    authorize(&site.policy, &self.principal, spec.permission(action)).await
  }

  /// [`allowed`](Self::allowed), or the error page (401, 403, or 500 when the policy failed).
  pub(crate) async fn require(
    &self,
    site: &SiteShared,
    spec: &EntitySpec,
    action: AdminAction,
  ) -> Result<(), Box<Response>> {
    self
      .allowed(site, spec, action)
      .await
      .map_err(|error| {
        Box::new(match error {
          AuthzError::Unauthenticated => {
            self.error(site, StatusCode::UNAUTHORIZED, "admin.forbidden")
          }
          AuthzError::Denied(_) => self.error(site, StatusCode::FORBIDDEN, "admin.forbidden"),
          other => {
            tracing::error!(target: "id_effect_admin", error = %other, "admin permission check failed");
            self.error(site, StatusCode::INTERNAL_SERVER_ERROR, "admin.error")
          }
        })
      })
  }

  /// The principal's id.
  pub(crate) fn principal_id(&self) -> &str {
    self.principal.id()
  }

  /// The request's tenant: a `CurrentTenant` request extension, else one recorded in the request
  /// capabilities.
  pub(crate) fn tenant(&self) -> Option<CurrentTenant> {
    if let Some(tenant) = self.parts.extensions.get::<CurrentTenant>() {
      return Some(tenant.clone());
    }
    self
      .parts
      .extensions
      .get::<RequestCaps>()
      .and_then(RequestCaps::get::<CurrentTenant>)
      .map(|tenant| CurrentTenant::clone(&tenant))
  }

  /// The repository of `E` bound to the request's tenant; a tenant-scoped entity without one is
  /// refused (403), never read unscoped.
  pub(crate) fn repository<E: Entity>(
    &self,
    site: &SiteShared,
  ) -> Result<Repository<E>, Box<Response>> {
    let repo = Repository::<E>::new(site.db.dialect());
    match (self.tenant(), E::TENANT) {
      (Some(tenant), _) => Ok(repo.with_tenant(tenant.0)),
      (None, None) => Ok(repo),
      (None, Some(_)) => Err(Box::new(self.error(
        site,
        StatusCode::FORBIDDEN,
        "admin.forbidden",
      ))),
    }
  }

  /// The label of `spec`'s entity.
  pub(crate) fn entity_label(&self, spec: &EntitySpec) -> String {
    self.labels.label(spec.label, || humanize(&spec.slug))
  }

  /// The label of field `at` of `spec`.
  pub(crate) fn field_label(&self, spec: &EntitySpec, at: usize) -> String {
    let field = &spec.fields[at];
    self.labels.label(field.label, || humanize(field.name))
  }

  /// The breadcrumb trail: the site, then `trail` (label and link; the last one is the page).
  pub(crate) fn crumbs(&self, site: &SiteShared, trail: &[(String, Option<String>)]) -> Markup {
    html! {
      nav.admin-crumbs aria-label="breadcrumb" {
        a href=(site.prefix) { (self.site_name(site)) }
        @for (label, href) in trail {
          " › "
          @if let Some(href) = href { a href=(href) { (label) } } @else { span aria-current="page" { (label) } }
        }
      }
    }
  }

  fn site_name(&self, site: &SiteShared) -> String {
    site
      .title
      .clone()
      .unwrap_or_else(|| self.labels.ui("admin"))
  }

  /// `content` as a page titled `title` with `status`: the fragment for an htmx request, else
  /// the page in the site's layout.
  pub(crate) fn page(
    &self,
    site: &SiteShared,
    title: &str,
    content: Markup,
    status: StatusCode,
  ) -> Response {
    let page = Page::new(title, content).status(status);
    match &site.layout {
      Some(layout) => respond(&**layout, &self.layout, page),
      None => {
        let layout = site.stylesheets.iter().fold(
          BasicLayout::new()
            .lang(self.labels.lang())
            .site_name(self.site_name(site)),
          |layout, href| layout.stylesheet(href.clone()),
        );
        respond(&layout, &self.layout, page)
      }
    }
  }

  /// An error page with `status` and the chrome message `key`.
  pub(crate) fn error(&self, site: &SiteShared, status: StatusCode, key: &str) -> Response {
    let title = status.canonical_reason().unwrap_or("Error");
    let content = html! {
      section.admin-error {
        (self.crumbs(site, &[]))
        h1 { (title) }
        p { (self.labels.ui(key)) }
      }
    };
    self.page(site, title, content, status)
  }
}

/// The index: the entities the principal may list.
async fn index(site: Arc<SiteShared>, req: Request) -> Response {
  let (parts, _body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  let mut visible = Vec::new();
  for spec in &site.entities {
    if ctx.allowed(&site, spec, AdminAction::List).await.is_ok() {
      visible.push((ctx.entity_label(spec), site.list_path(&spec.slug)));
    }
  }
  let name = ctx.site_name(&site);
  let content = html! {
    section.admin-index {
      h1 { (name) }
      @if visible.is_empty() {
        p.admin-empty { (ctx.labels.ui("admin.nothing")) }
      } @else {
        ul.admin-entities {
          @for (label, href) in &visible { li { a href=(href) { (label) } } }
        }
      }
    }
  };
  ctx.page(&site, &name, content, StatusCode::OK)
}
