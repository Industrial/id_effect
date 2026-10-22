//! Bulk actions over the records selected on a list page (web.admin #5): the built-in `delete`
//! and the custom [`BulkAction`]s an entity registers ([`EntityAdmin::action`]).
//!
//! The list page shows a checkbox per row and an action menu when the principal holds
//! `admin.<entity>.bulk` and at least one action's own permission (`delete` needs
//! `admin.<entity>.delete`; a custom action needs the one it [`requires`](BulkAction::requires),
//! `update` by default). The form posts `action=<name>` and one `id=<id>` per selected row to
//! `POST <prefix>/<entity>/@bulk`.
//!
//! **Secure by default** (ADR 0013 R1, R4, R9): at most [`MAX_BULK_IDS`] ids per request (more
//! is refused with 400 before any query), both permissions are checked, the CSRF check of the
//! `SecurityLayer` must have passed, and the selected records are read through the request's
//! tenant-bound repository, so an id of another tenant selects nothing. The action and its audit
//! record (feature `audit`) run in one transaction; deletes go through the entity's hooks.
//!
//! ```
//! use id_effect::succeed;
//! use id_effect_admin::bulk::BulkAction;
//! use id_effect_admin::{AdminAction, EntityAdmin};
//! use id_effect_sql::{Entity, FromRow};
//!
//! #[derive(Clone, Debug, FromRow, Entity)]
//! #[entity(table = "documents")]
//! struct Document {
//!   #[entity(id)]
//!   id: i64,
//!   #[entity(editable)]
//!   status: String,
//! }
//!
//! let publish = BulkAction::new("publish", "Publish", |ctx, rows: Vec<Document>| {
//!   rows.into_iter().fold(succeed(()), |done, mut doc| {
//!     doc.status = "published".into();
//!     let update = ctx.update(doc);
//!     done.flat_map(move |()| update.map(|_| ()))
//!   })
//! })
//! .requires(AdminAction::Update);
//! let admin = EntityAdmin::<Document>::default().action(publish);
//! assert_eq!(admin.info().unwrap().actions, ["publish"]);
//! ```

use std::fmt;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use id_effect::{Effect, succeed};
use id_effect_axum::ui::{Markup, html};
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait, Value};
use id_effect_sql::{
  BoundRepository, Entity, Repository, SqlError, SqlTransaction, SqlTransactionService,
  with_transaction,
};

use crate::audit::{self, Actor, Change};
use crate::edit::{AdminRoute, Typed, Writer, authorize_write, csrf_input, id_of, read_body, run};
use crate::entity::{EntityAdmin, EntitySpec};
use crate::permission::AdminAction;
use crate::site::{Ctx, SiteShared};

/// The most records one bulk action may select.
pub const MAX_BULK_IDS: usize = 100;

/// The name of the built-in bulk delete.
pub const DELETE: &str = "delete";

type RunAction<E> = Arc<dyn Fn(&BulkContext<E>, Vec<E>) -> Effect<(), SqlError> + Send + Sync>;

/// A custom bulk action: a name (`snake_case`, in the form and the audit log), a label (a
/// translation key, or the text itself), the permission it needs besides
/// `admin.<entity>.bulk`, and what it does with the selected records.
pub struct BulkAction<E> {
  name: String,
  label: String,
  requires: AdminAction,
  run: RunAction<E>,
}

impl<E> Clone for BulkAction<E> {
  fn clone(&self) -> Self {
    Self {
      name: self.name.clone(),
      label: self.label.clone(),
      requires: self.requires,
      run: Arc::clone(&self.run),
    }
  }
}

impl<E> fmt::Debug for BulkAction<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("BulkAction")
      .field("name", &self.name)
      .field("label", &self.label)
      .field("requires", &self.requires)
      .finish_non_exhaustive()
  }
}

impl<E> BulkAction<E> {
  /// The action `name` shown as `label`, running `run` on the selected records (read in the
  /// request's tenant) inside the action's transaction. It requires
  /// `admin.<entity>.update` unless [`requires`](Self::requires) names another action.
  pub fn new(
    name: impl Into<String>,
    label: impl Into<String>,
    run: impl Fn(&BulkContext<E>, Vec<E>) -> Effect<(), SqlError> + Send + Sync + 'static,
  ) -> Self {
    Self {
      name: name.into(),
      label: label.into(),
      requires: AdminAction::Update,
      run: Arc::new(run),
    }
  }

  /// The entity permission the action needs besides `admin.<entity>.bulk`.
  #[must_use]
  pub fn requires(mut self, action: AdminAction) -> Self {
    self.requires = action;
    self
  }

  /// The action's name.
  pub fn name(&self) -> &str {
    &self.name
  }

  pub(crate) fn spec(&self) -> ActionSpec {
    ActionSpec {
      name: self.name.clone(),
      label: self.label.clone(),
      requires: self.requires,
    }
  }
}

/// A custom action, checked (non-generic).
#[derive(Clone, Debug)]
pub(crate) struct ActionSpec {
  pub(crate) name: String,
  pub(crate) label: String,
  pub(crate) requires: AdminAction,
}

/// What a [`BulkAction`] works with: the action's transaction, the entity's repository bound to
/// it (in the request's tenant), and the acting principal.
pub struct BulkContext<E> {
  repo: Repository<E>,
  tx: Arc<dyn SqlTransaction>,
  writer: Writer<E>,
  principal: String,
}

impl<E> fmt::Debug for BulkContext<E> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("BulkContext")
      .field("principal", &self.principal)
      .finish_non_exhaustive()
  }
}

impl<E: Entity> BulkContext<E> {
  /// The action's transaction (commits with the audit record, or rolls back with it).
  pub fn transaction(&self) -> &Arc<dyn SqlTransaction> {
    &self.tx
  }

  /// The entity's tenant-bound repository on the action's transaction (writes through it skip
  /// the entity's hooks; [`update`](Self::update) runs them).
  pub fn repository(&self) -> BoundRepository<E> {
    self.repo.on(SqlTransactionService(Arc::clone(&self.tx)))
  }

  /// Update `record` the way the admin's edit form does: through the entity's hooks, in the
  /// action's transaction.
  pub fn update(&self, record: E) -> Effect<E, SqlError> {
    self.writer.update(&self.repo, &self.tx, record)
  }

  /// The acting principal's id.
  pub fn principal(&self) -> &str {
    &self.principal
  }
}

impl<E> EntityAdmin<E> {
  /// Offer the bulk action `action` on the list page. Its name must be snake_case ASCII, unique
  /// for the entity and not [`DELETE`]
  /// ([`InvalidAction`](crate::AdminConfigError::InvalidAction) otherwise).
  #[must_use]
  pub fn action(mut self, action: BulkAction<E>) -> Self {
    self.edit.actions.push(action);
    self
  }
}

/// `<prefix>/<entity>/@bulk`.
fn bulk_path(site: &SiteShared, slug: &str) -> String {
  format!("{}/@bulk", site.list_path(slug))
}

/// The route `POST <prefix>/<entity>/@bulk`.
pub(crate) fn route<E: Entity>(
  site: &Arc<SiteShared>,
  spec: &Arc<EntitySpec>,
  typed: &Arc<Typed<E>>,
) -> AdminRoute {
  let (site, spec, typed) = (Arc::clone(site), Arc::clone(spec), Arc::clone(typed));
  AdminRoute::new(
    Method::POST,
    bulk_path(&site, &spec.slug),
    "id_effect_admin::bulk",
    axum::routing::post(move |req: Request| {
      bulk::<E>(
        Arc::clone(&site),
        Arc::clone(&spec),
        Arc::clone(&typed),
        req,
      )
    }),
  )
}

/// The actions the principal may run on `spec`'s list page: `(name, label)`.
pub(crate) async fn options(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
) -> Vec<(String, String)> {
  let mut options = Vec::new();
  if ctx.allowed(site, spec, AdminAction::Bulk).await.is_err() {
    return options;
  }
  if ctx.allowed(site, spec, AdminAction::Delete).await.is_ok() {
    options.push((DELETE.to_owned(), ctx.labels.ui("admin.delete")));
  }
  for action in &spec.edit.actions {
    if ctx.allowed(site, spec, action.requires).await.is_ok() {
      let label = ctx.labels.label(&action.label, || action.label.clone());
      options.push((action.name.clone(), label));
    }
  }
  options
}

/// The selection checkbox of the row `id`.
pub(crate) fn checkbox(ctx: &Ctx, id: &str) -> Markup {
  html! { input type="checkbox" name="id" value=(id) aria-label=(ctx.labels.ui("admin.select")); }
}

/// `table` inside the bulk form with its action menu.
pub(crate) fn form(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  options: &[(String, String)],
  table: Markup,
) -> Markup {
  html! {
    form.admin-bulk method="post" action=(bulk_path(site, &spec.slug)) {
      (csrf_input(ctx))
      (table)
      div.admin-bulk-actions {
        label {
          (ctx.labels.ui("admin.selected")) " "
          select name="action" {
            @for (name, label) in options { option value=(name) { (label) } }
          }
        }
        " "
        button type="submit" { (ctx.labels.ui("admin.run")) }
      }
    }
  }
}

/// The rows of `E` with the primary keys `ids`, through `repo` (its tenant).
fn select<E: Entity>(
  repo: &Repository<E>,
  site: &SiteShared,
  ids: Vec<id_effect_sql::SqlValue>,
) -> Effect<Vec<E>, SqlError> {
  repo
    .on(site.db.client())
    .find_by(Expr::col(E::ID).is_in(ids.into_iter().map(Value::from)))
}

/// `POST <prefix>/<entity>/@bulk`.
async fn bulk<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  typed: Arc<Typed<E>>,
  req: Request,
) -> Response {
  let (parts, body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  let done = async {
    authorize_write(&ctx, &site, &spec, &[AdminAction::Bulk]).await?;
    let repo = ctx.repository::<E>(&site)?;
    let body = read_body(&ctx, &site, body).await?;
    let bad = || Box::new(ctx.error(&site, StatusCode::BAD_REQUEST, "admin.bad-request"));
    let texts: Vec<&str> = body.values("id").collect();
    if texts.len() > MAX_BULK_IDS {
      return Err(Box::new(ctx.error(
        &site,
        StatusCode::BAD_REQUEST,
        "admin.too-many",
      )));
    }
    let name = body.value("action").unwrap_or_default();
    let custom = typed.actions.iter().find(|a| a.name == name).cloned();
    let required = match &custom {
      Some(action) => action.requires,
      None if name == DELETE => AdminAction::Delete,
      None => return Err(bad()),
    };
    ctx.require(&site, &spec, required).await?;
    let ids = texts
      .iter()
      .map(|t| spec.id_kind.parse(t))
      .collect::<Option<Vec<_>>>()
      .ok_or_else(bad)?;
    let list = Redirect::to(&site.list_path(&spec.slug)).into_response();
    if ids.is_empty() {
      return Ok(list);
    }
    let read = select::<E>(&repo, &site, ids);
    let rows = run(&ctx, &site, &spec, read).await?;
    let selected: Vec<String> = rows.iter().map(|r| id_of(&spec, r)).collect();
    let change = Change::bulk(name, &spec.slug, selected);
    let (actor, writer, audit_site) = (Actor::of(&ctx), typed.writer.clone(), Arc::clone(&site));
    let principal = ctx.principal_id().to_owned();
    let write = with_transaction(site.db.client().0, move |tx| {
      let act = match custom {
        Some(action) => {
          let context = BulkContext {
            repo,
            tx: Arc::clone(&tx),
            writer,
            principal,
          };
          (action.run)(&context, rows)
        }
        None => rows.into_iter().fold(succeed(()), |done, row| {
          let delete = writer.delete(&repo, &tx, row);
          done.flat_map(move |()| delete.map(|_| ()))
        }),
      };
      act.flat_map(move |()| audit::record(&audit_site, &actor, &tx, change))
    });
    run(&ctx, &site, &spec, write).await?;
    Ok::<_, Box<Response>>(list)
  };
  done.await.unwrap_or_else(|r| *r)
}
