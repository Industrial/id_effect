//! The admin back office (id_effect_kit::admin, D1 stage 5): a module per entity (its list
//! columns, search box and filters), each registered in [`site`] with its permissions listed in
//! [`permissions`]. `id-effect generate admin <Entity>` adds the module and its lines; the
//! README section "Back office" lists the commands and every edit made after generation.
//!
//! Secure by default: [`router`] mounts the site under [`PREFIX`] only behind a guard (there is
//! no public admin), and every page checks its own permission, `admin.<entity>.<action>`, with
//! [`policy`] before any query. Nobody holds one until a role is granted it, and a permission no
//! entity's admin offers (creating a document) is refused whatever the grants say.
//!
//! The Pipeline mounts one site per tenant ([`routes`]): `/admin/<tenant>`, behind [`guard`] (a
//! `reviewer` or an `admin` whose tenant claim is that tenant), reading that tenant's own
//! database through the tenant router and its rows only (the request's `CurrentTenant`). The
//! roles ([`rbac`]):
//!
//! | Role | May |
//! |---|---|
//! | `reviewer` | list and view sources, documents and labels; update documents (their title and the label picker); bulk re-run extraction |
//! | `admin` | everything the admins offer |
//!
//! Every create, update, delete, label change and bulk action is written to the hash-chained
//! audit log of the tenant's database, in the change's own transaction (kit feature `audit`).
//!
//! ```text
//! let staff = admin::permissions()?.iter().map(ToString::to_string).collect::<Vec<_>>();
//! let rbac = Rbac::builder().grant("staff", staff).build();
//! let guard = Guard::new("staff", require_role("staff"));
//! let routes = admin::router(&app, "acme", guard, rbac, AdminDatabase::new(sql_client, Dialect::Postgres))?;
//! ```

pub mod document;
pub mod label;
pub mod source;

use std::sync::Arc;

use id_effect::DynEnv;
use id_effect_kit::admin::{
  AdminAction, AdminConfigError, AdminDatabase, AdminPermission, AdminSite, Guarded,
  RbacAdminPolicy,
};
use id_effect_kit::authz::{
  CurrentPrincipal, Guard, Policy, PolicyExt, Rbac, predicate, require_any_role,
};
use id_effect_kit::axum::route::{RouteGuard, Router};
use id_effect_kit::i18n::Catalog;

use crate::App;
use crate::domain::DIALECT;
use crate::tenancy::ROLE_REVIEWER;

/// Where the admin's pages are mounted (each tenant's site under `/admin/<tenant>`).
pub const PREFIX: &str = "/admin";

/// Administers everything the admins offer (the `reviewer` role is the other one).
pub const ROLE_ADMIN: &str = "admin";

/// Every entity's admin for `tenant` of `app`, on a site that is not guarded yet ([`router`]
/// guards and mounts it).
pub fn site(app: &Arc<App>, tenant: &str) -> Result<AdminSite, AdminConfigError> {
  let site = AdminSite::new().prefix(PREFIX);
  let site = source::register(site)?;
  let site = document::register(site, app, tenant)?;
  let site = label::register(site)?;
  Ok(site)
}

/// The permissions the entities' admins offer: the ones a role can be granted.
pub fn permissions() -> Result<Vec<AdminPermission>, AdminConfigError> {
  let permissions = Vec::new();
  let permissions = [permissions, source::permissions()?].concat();
  let permissions = [permissions, document::permissions()?].concat();
  let permissions = [permissions, label::permissions()?].concat();
  Ok(permissions)
}

/// Decides every admin permission: allowed only when an entity's admin offers it and `rbac`
/// grants it to the principal (through a role, or as one of the principal's own permissions).
/// Anything else, and every permission of an anonymous principal, is denied.
pub fn policy(rbac: Rbac) -> Result<impl Policy<AdminPermission>, AdminConfigError> {
  let offered = permissions()?;
  let offered = predicate(
    "not offered by this admin",
    move |_: &CurrentPrincipal, permission: &AdminPermission| offered.contains(permission),
  );
  Ok(offered.and(RbacAdminPolicy::new(rbac)))
}

/// The admin's routes of `tenant` under [`PREFIX`], every one behind `guard` (who may enter at
/// all); each page then checks its permission with [`policy`] over `rbac`. Merge them into the
/// application's router, inside its authentication and `SecurityLayer` (CSRF) layers.
pub fn router<S: Clone + Send + Sync + 'static>(
  app: &Arc<App>,
  tenant: &str,
  guard: impl RouteGuard,
  rbac: Rbac,
  db: AdminDatabase,
) -> Result<Router<S>, AdminConfigError> {
  let site = site(app, tenant)?.policy(policy(rbac)?).guard(guard);
  Ok(site.router(db))
}

// ---- the Pipeline's tenants and roles (added after generation) ----

/// Where `tenant`'s admin is mounted: `/admin/<tenant>`.
pub fn tenant_prefix(tenant: &str) -> String {
  format!("{PREFIX}/{tenant}")
}

/// Whether a `reviewer` holds `permission`: list and view everything, update documents (their
/// title and labels) and run bulk actions on them.
pub fn reviewer_may(permission: &AdminPermission) -> bool {
  match permission.action() {
    AdminAction::List | AdminAction::View => true,
    AdminAction::Update | AdminAction::Bulk => permission.entity() == document::SLUG,
    _ => false,
  }
}

/// The grants: `reviewer` as [`reviewer_may`] says, `admin` every offered permission.
///
/// # Errors
///
/// When an entity's admin does not build (a defect).
pub fn rbac() -> Result<Rbac, AdminConfigError> {
  let all = permissions()?;
  let reviewer: Vec<String> = all
    .iter()
    .filter(|p| reviewer_may(p))
    .map(ToString::to_string)
    .collect();
  Ok(
    Rbac::builder()
      .grant(ROLE_REVIEWER, reviewer)
      .grant(ROLE_ADMIN, all.iter().map(ToString::to_string))
      .build(),
  )
}

/// Who may enter `tenant`'s admin at all: a `reviewer` or an `admin` whose tenant claim is
/// `tenant` (401 for an anonymous caller, 403 otherwise). Each page then checks its permission.
pub fn guard(tenant: &str) -> Guard {
  let tenant = tenant.to_owned();
  let same = predicate(
    "the principal belongs to another tenant",
    move |p: &CurrentPrincipal, _: &()| p.tenant() == Some(tenant.as_str()),
  );
  Guard::new(
    "pipeline.admin",
    require_any_role([ROLE_REVIEWER, ROLE_ADMIN]).and(same),
  )
}

/// `tenant`'s guarded site: under [`tenant_prefix`], with [`policy`] over [`rbac`], labels and
/// page text from `catalog` in the request's locale, behind [`guard`].
///
/// # Errors
///
/// When an entity's admin does not build (a defect).
pub fn tenant_site(
  app: &Arc<App>,
  tenant: &str,
  catalog: &Catalog,
) -> Result<AdminSite<Guarded>, AdminConfigError> {
  Ok(
    site(app, tenant)?
      .prefix(tenant_prefix(tenant))
      .policy(policy(rbac()?)?)
      .catalog(catalog.clone())
      .guard(guard(tenant)),
  )
}

/// Every tenant's admin as typed routes: [`tenant_site`] reading the tenant's own database
/// (`TenantRouter::for_tenant`).
///
/// # Errors
///
/// When an entity's admin does not build (a defect).
pub fn routes(app: &Arc<App>, catalog: &Catalog) -> Result<Router<DynEnv>, AdminConfigError> {
  app
    .tenants
    .iter()
    .try_fold(Router::new(), |router, tenant| {
      let db = AdminDatabase::new(Arc::new(app.router.for_tenant(&tenant.id)), DIALECT);
      Ok(router.merge(tenant_site(app, &tenant.id, catalog)?.router(db)))
    })
}

#[cfg(test)]
pub(crate) mod tests {
  use std::sync::Arc;

  use id_effect_kit::ai::testing::ScriptedModel;
  use id_effect_kit::authz::{BasicPrincipal, Decision};
  use id_effect_kit::sql::TestSqlClient;
  use id_effect_kit::tenancy::TenantRouter;

  use super::*;
  use crate::tenancy::{TenantConfig, Tenants};

  /// An app over test doubles (nothing connects): what the admin's configuration needs.
  pub(crate) fn test_app() -> Arc<App> {
    let tenants = Tenants::new([TenantConfig::new("acme", "en").expect("acme")]);
    let router = TenantRouter::from_map(Vec::<(String, _)>::new());
    Arc::new(App::new(
      tenants,
      router,
      Arc::new(TestSqlClient::new()),
      Arc::new(ScriptedModel::new()),
      Arc::new(ScriptedModel::new()),
    ))
  }

  #[test]
  fn reviewers_hold_the_review_permissions_and_admins_all() {
    let policy = policy(rbac().unwrap()).unwrap();
    let reviewer = CurrentPrincipal::new(BasicPrincipal::new("rita").with_role(ROLE_REVIEWER));
    let admin = CurrentPrincipal::new(BasicPrincipal::new("ada").with_role(ROLE_ADMIN));
    let allowed = |who: &CurrentPrincipal, entity: &str, action: AdminAction| {
      let permission = AdminPermission::new(entity, action).unwrap();
      policy.decide_now(who, &permission) == Some(Decision::Allow)
    };
    assert!(allowed(&reviewer, "document", AdminAction::Update));
    assert!(allowed(&reviewer, "document", AdminAction::Bulk));
    assert!(allowed(&reviewer, "label", AdminAction::List));
    assert!(!allowed(&reviewer, "label", AdminAction::Update));
    assert!(!allowed(&reviewer, "document", AdminAction::Delete));
    assert!(allowed(&admin, "label", AdminAction::Create));
    assert!(allowed(&admin, "document", AdminAction::Delete));
    // Nobody creates documents in the admin: they arrive through ingestion.
    assert!(!allowed(&admin, "document", AdminAction::Create));
  }

  #[test]
  fn every_tenant_site_is_guarded_under_its_prefix() {
    let app = test_app();
    let routes = routes(&app, &crate::ui::catalog()).unwrap();
    let report = routes.access_report();
    assert!(report.public.is_empty(), "{:?}", report.public);
    assert!(
      report
        .guarded
        .iter()
        .any(|(route, _)| route.as_str() == "GET /admin/acme/document")
    );
    assert_eq!(tenant_prefix("globex"), "/admin/globex");
  }
}
