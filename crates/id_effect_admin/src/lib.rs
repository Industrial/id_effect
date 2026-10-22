//! Admin back office for id_effect services (ADR 0010 Amendment 9, release spec 0.13.0 decision
//! D2): register entities on an [`AdminSite`], put it behind a guard, and get list and detail
//! pages generated from the entities' field metadata — search, typed filters, sortable columns,
//! keyset pages — rendered with maud and updated in place by htmx.
//!
//! ```
//! use std::sync::Arc;
//!
//! use axum::body::Body;
//! use axum::http::{Request, StatusCode};
//! use id_effect_admin::{AdminDatabase, AdminPermission, AdminSite, EntityAdmin, Filter};
//! use id_effect_authz::{BasicPrincipal, CurrentPrincipal, Guard, Rbac, require_role};
//! use id_effect_sql::{Dialect, Entity, FromRow, TestSqlClient};
//! use tower::ServiceExt;
//!
//! #[derive(Clone, Debug, FromRow, Entity)]
//! #[entity(table = "documents")]
//! struct Document {
//!   #[entity(id)]
//!   id: i64,
//!   title: String,
//!   status: String,
//! }
//!
//! let rbac = Rbac::builder()
//!   .grant("editor", AdminPermission::all_for("document").unwrap().iter().map(|p| p.to_string()))
//!   .build();
//! let app: axum::Router = AdminSite::new()
//!   .register::<Document>(
//!     EntityAdmin::default()
//!       .list(["id", "title", "status"])
//!       .search(["title"])
//!       .filter("status", Filter::choices(["draft", "published"]))
//!       .sortable(["title"]),
//!   )
//!   .rbac(rbac)
//!   .guard(Guard::new("staff", require_role("editor")))
//!   .router::<()>(AdminDatabase::new(Arc::new(TestSqlClient::new()), Dialect::Sqlite))
//!   .into_axum();
//!
//! // An authentication layer puts the principal on the request.
//! let ed = CurrentPrincipal::new(BasicPrincipal::new("ed").with_role("editor"));
//! let mut req = Request::get("/admin").body(Body::empty()).unwrap();
//! req.extensions_mut().insert(ed);
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let res = app.oneshot(req).await.unwrap();
//! assert_eq!(res.status(), StatusCode::OK);
//! let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
//! let body = String::from_utf8(body.to_vec()).unwrap();
//! assert!(body.contains(r#"<a href="/admin/document">Document</a>"#));
//! # });
//! ```
//!
//! ## Secure by default (ADR 0013 R1, R7, R9)
//!
//! - **No public admin.** A site mounts only after [`AdminSite::guard`]; an unguarded site has no
//!   [`router`](AdminSite::router) (a compile error), and every route it mounts is registered as
//!   guarded in the application's route registry.
//! - **Every action is authorized.** Each entity × action is its own permission,
//!   `admin.<entity>.<list|view|create|update|delete|bulk>` ([`AdminPermission`]), decided by the
//!   site's `id_effect_authz` policy before any query runs; a principal holds none unless granted.
//! - **Rows stay in their tenant.** Pages read through a repository bound to the request's
//!   `CurrentTenant`; a tenant-scoped entity without a tenant is refused, never read unscoped.
//! - **Escaped output.** Pages are maud templates: every value is escaped, and this crate never
//!   builds pre-escaped markup itself (raw HTML goes only through
//!   `id_effect_axum::ui::trusted_html`; the test `no_preescaped_outside_trusted_module` greps the
//!   sources).
//! - **Signed-in principals only.** Even behind a guard that admits anonymous callers, an
//!   anonymous principal is refused (401) before the policy is asked.
//! - **Bounded input.** Query strings, search text, filter values and page cursors have limits
//!   ([`list`]); a form body is at most 64 KiB (413 above) and a bulk action at most 100 records.
//! - **No mass assignment.** Forms bind only the fields declared `#[entity(editable)]`; any other
//!   submitted field is ignored and logged ([`forms`]).
//! - **CSRF on every write.** A `POST` must have passed the masked-token check of
//!   `id_effect_axum`'s `SecurityLayer`; without it the admin refuses to write (403).
//! - **Every change audited** (feature `audit`): in the write's transaction, into the
//!   hash-chained audit log ([`audit`]).
//!
//! ## Modules
//!
//! | Module | What |
//! |---|---|
//! | [`site`] | [`AdminSite`]: registration, guard typestate, routes |
//! | [`entity`] | [`EntityAdmin`]: columns, search, filters, sort, page size |
//! | [`permission`] | [`AdminPermission`], [`RbacAdminPolicy`] |
//! | [`list`] | the list page's query parameters and limits |
//! | [`edit`] | create, edit and delete pages: validation, hooks, optimistic locking |
//! | [`forms`] | the bounded form decoder and the mass-assignment guard |
//! | [`relations`] | `belongs_to` pickers, `has_many` / `many_to_many` inline lists |
//! | [`bulk`] | bulk delete and custom [`BulkAction`]s (at most 100 records) |
//! | [`audit`] | every change in the hash-chained audit log (feature `audit`) |
//! | [`labels`] | labels and page text, translated with feature `i18n` |
//! | `testing` | requests with a principal and tenant (feature `test-util`) |

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod audit;
pub mod bulk;
mod detail;
pub mod edit;
pub mod entity;
pub mod forms;
pub mod labels;
pub mod list;
pub mod permission;
pub mod relations;
pub mod site;
#[cfg(feature = "test-util")]
pub mod testing;
mod value;

pub use bulk::{BulkAction, BulkContext};
pub use entity::{AdminConfigError, EntityAdmin, EntityInfo, Filter};
pub use permission::{AdminAction, AdminPermission, AdminPermissionError, RbacAdminPolicy};
pub use site::{AdminDatabase, AdminSite, Guarded, SiteAccess, Unguarded};

/// Compiles and runs the README quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
