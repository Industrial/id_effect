//! Create, edit and delete pages (web.admin #1, write half), generated from the entity's field
//! metadata ([`Entity::FIELDS`]).
//!
//! | Route | Page | Permission |
//! |---|---|---|
//! | `GET`, `POST <prefix>/<entity>/@new` | the create form; create | `admin.<entity>.create` |
//! | `GET`, `POST <prefix>/<entity>/{id}/@edit` | the edit form; update | `admin.<entity>.update` |
//! | `GET`, `POST <prefix>/<entity>/{id}/@delete` | the confirmation; delete | `admin.<entity>.delete` |
//!
//! (`@` never appears unencoded in an id the admin links to, so these paths cannot shadow a
//! record.)
//!
//! - **Fields.** The forms bind the entity's editable fields (`#[entity(editable)]`), or the
//!   subset [`EntityAdmin::form`] lists; the primary key, the version, tenant and soft-delete
//!   columns are never bound (see [`forms`](crate::forms) for the mass-assignment guard). A
//!   create form also asks for the primary key when the database does not generate it. The other
//!   fields of a new record come from [`EntityAdmin::new_record`], else NULL (nullable fields) or
//!   the column's zero (generated id, version); an entity with a required field that is neither
//!   editable nor covered has no create page. A form field an update request leaves out keeps
//!   its stored value.
//! - **Validation.** Each value is parsed as its field's type, then the entity is decoded from the
//!   row (`#[from_row(schema)]` runs its `Schema`'s refinements), then
//!   [`EntityAdmin::validate`] runs. Every issue is bound to its field through the `ui` form
//!   helpers (`FormState`) and the form is answered again with 422; with feature `i18n` and a
//!   catalog, the messages are translated in the request's locale (the schema messages of
//!   `id_effect_i18n`). A `belongs_to` foreign key must name a row of the target entity in the
//!   request's tenant (when the target is registered).
//! - **Writes go through the repository**, in one transaction with the audit record: the
//!   entity's [`EntityAdmin::hooks`] (`id_effect_sql` lifecycle hooks) run inside it, a failing
//!   hook rolls the write back, and optimistic locking applies (the edit form carries the version
//!   it was rendered with; a concurrent change answers 409).
//! - **CSRF.** Every `POST` must have passed the CSRF check of `id_effect_axum`'s
//!   `SecurityLayer` (a masked token in the form or the `x-csrf-token` header); a request that did
//!   not (no `CsrfToken` on it) is refused with 403 whatever its body says. Forms render the
//!   token.
//! - **Delete asks first:** `GET …/@delete` shows a confirmation whose button posts the delete.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Request};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::MethodRouter;
use id_effect::schema::ParseErrors;
use id_effect::{Cause, Effect, Exit};
use id_effect_axum::EffectExecutor;
use id_effect_axum::server::security::CsrfToken;
use id_effect_axum::ui::form::{FormState, csrf_field};
use id_effect_axum::ui::{Markup, html};
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait, Value};
use id_effect_sql::repository::EntityHooks;
use id_effect_sql::{
  Entity, FieldMeta, Repository, SQLSTATE_UNIQUE_VIOLATION, SqlError, SqlRow, SqlTransaction,
  SqlValue, with_transaction,
};

use crate::audit::{self, Actor, Change};
use crate::bulk::{ActionSpec, BulkAction};
use crate::entity::{AdminConfigError, EntityAdmin, EntitySpec};
use crate::forms::{self, FormBody};
use crate::permission::AdminAction;
use crate::relations::{self, RelationSpec};
use crate::site::{Ctx, SiteShared};
use crate::value::{ValueKind, id_text};

/// Validation of a decoded record ([`EntityAdmin::validate`]).
pub(crate) type Validator<E> = Arc<dyn Fn(&E) -> Result<(), ParseErrors> + Send + Sync>;
/// The record a create form starts from ([`EntityAdmin::new_record`]).
pub(crate) type Template<E> = Arc<dyn Fn() -> E + Send + Sync>;

type Save<E> = Arc<
  dyn Fn(&Repository<E>, &Arc<dyn SqlTransaction>, E, bool) -> Effect<E, SqlError> + Send + Sync,
>;
type Remove<E> =
  Arc<dyn Fn(&Repository<E>, &Arc<dyn SqlTransaction>, E) -> Effect<bool, SqlError> + Send + Sync>;

/// How the admin writes `E`: through a repository with the entity's lifecycle hooks, inside the
/// caller's transaction.
pub(crate) struct Writer<E> {
  save: Save<E>,
  remove: Remove<E>,
}

impl<E> Clone for Writer<E> {
  fn clone(&self) -> Self {
    Self {
      save: Arc::clone(&self.save),
      remove: Arc::clone(&self.remove),
    }
  }
}

impl<E> Writer<E> {
  pub(crate) fn insert(
    &self,
    repo: &Repository<E>,
    tx: &Arc<dyn SqlTransaction>,
    entity: E,
  ) -> Effect<E, SqlError> {
    (self.save)(repo, tx, entity, true)
  }

  pub(crate) fn update(
    &self,
    repo: &Repository<E>,
    tx: &Arc<dyn SqlTransaction>,
    entity: E,
  ) -> Effect<E, SqlError> {
    (self.save)(repo, tx, entity, false)
  }

  pub(crate) fn delete(
    &self,
    repo: &Repository<E>,
    tx: &Arc<dyn SqlTransaction>,
    entity: E,
  ) -> Effect<bool, SqlError> {
    (self.remove)(repo, tx, entity)
  }
}

/// The writer running `hooks` (in a savepoint of the admin's transaction).
fn writer<E: Entity>(hooks: EntityHooks<E>) -> Writer<E> {
  let remove_hooks = hooks.clone();
  Writer {
    save: Arc::new(move |repo, tx, entity, insert| {
      let hooked = repo.with_hooks(hooks.clone()).in_transaction(tx);
      if insert {
        hooked.insert(entity)
      } else {
        hooked.update(entity)
      }
    }),
    remove: Arc::new(move |repo, tx, entity| {
      repo
        .with_hooks(remove_hooks.clone())
        .in_transaction(tx)
        .delete(entity.id())
    }),
  }
}

/// The editing options of an [`EntityAdmin`], as its builder methods set them.
pub(crate) struct EditOptions<E> {
  form: Option<Vec<String>>,
  writer: Option<Writer<E>>,
  validate: Option<Validator<E>>,
  template: Option<Template<E>>,
  pub(crate) actions: Vec<BulkAction<E>>,
}

impl<E> Default for EditOptions<E> {
  fn default() -> Self {
    Self {
      form: None,
      writer: None,
      validate: None,
      template: None,
      actions: Vec::new(),
    }
  }
}

impl<E> Clone for EditOptions<E> {
  fn clone(&self) -> Self {
    Self {
      form: self.form.clone(),
      writer: self.writer.clone(),
      validate: self.validate.clone(),
      template: self.template.clone(),
      actions: self.actions.clone(),
    }
  }
}

impl<E> EntityAdmin<E> {
  /// The fields the create and edit forms show and bind, in order (default: every editable
  /// field that is not excluded). Each must be declared `#[entity(editable)]` and of a type a
  /// form parses ([`AdminConfigError::NotEditable`] otherwise): this list narrows the editable
  /// fields, it never widens them.
  #[must_use]
  pub fn form<S: Into<String>>(mut self, fields: impl IntoIterator<Item = S>) -> Self {
    self.edit.form = Some(fields.into_iter().map(Into::into).collect());
    self
  }

  /// Check every created or updated record with `validate` after it decoded: the issues of the
  /// returned `ParseErrors` (a path naming a field binds to it) are shown on the form, which is
  /// answered with 422.
  #[must_use]
  pub fn validate(
    mut self,
    validate: impl Fn(&E) -> Result<(), ParseErrors> + Send + Sync + 'static,
  ) -> Self {
    self.edit.validate = Some(Arc::new(validate));
    self
  }

  /// The record a create form starts from: its values fill the form, and every field the form
  /// does not bind (the generated id is assigned by the database, the tenant by the repository).
  #[must_use]
  pub fn new_record(mut self, record: impl Fn() -> E + Send + Sync + 'static) -> Self {
    self.edit.template = Some(Arc::new(record));
    self
  }
}

impl<E: Entity> EntityAdmin<E> {
  /// Write through a repository with `hooks`: every admin insert, update and delete (bulk ones
  /// included) runs them inside its transaction, and a failing hook rolls the write back
  /// (`id_effect_sql` lifecycle hooks, data.orm #7). Without hooks the admin writes through the
  /// plain repository, in the same transaction as its audit record.
  #[must_use]
  pub fn hooks(mut self, hooks: EntityHooks<E>) -> Self {
    self.edit.writer = Some(writer(hooks));
    self
  }
}

/// The checked editing configuration of an entity (non-generic; part of its `EntitySpec`).
#[derive(Clone, Debug)]
pub(crate) struct EditSpec {
  /// The bound fields.
  pub(crate) form: Vec<usize>,
  /// The create form asks for the primary key (not generated by the database).
  pub(crate) create_id: bool,
  /// A record can be created.
  pub(crate) creatable: bool,
  /// The version field.
  pub(crate) version: Option<usize>,
  /// The field naming a record in pickers and inline lists.
  pub(crate) display: usize,
  /// The custom bulk actions.
  pub(crate) actions: Vec<ActionSpec>,
  /// The declared relations.
  pub(crate) relations: Vec<RelationSpec>,
}

impl EditSpec {
  pub(crate) fn check<E: Entity>(
    options: &EditOptions<E>,
    exclude: &[String],
    search: &[&'static str],
  ) -> Result<Self, AdminConfigError> {
    let entity = E::NAME;
    let fields = E::FIELDS;
    let excluded = |at: usize| exclude.iter().any(|x| x == fields[at].name);
    let bindable = |at: usize| fields[at].editable && !excluded(at) && forms::parses(&fields[at]);
    let form = match &options.form {
      Some(names) => names
        .iter()
        .map(|name| {
          let at = fields.iter().position(|f| f.name == name).ok_or_else(|| {
            AdminConfigError::UnknownField {
              entity,
              field: name.clone(),
            }
          })?;
          if bindable(at) {
            Ok(at)
          } else {
            Err(AdminConfigError::NotEditable {
              entity,
              field: name.clone(),
            })
          }
        })
        .collect::<Result<Vec<_>, _>>()?,
      None => (0..fields.len()).filter(|at| bindable(*at)).collect(),
    };
    let column_at = |column: &str| fields.iter().position(|f| f.column == column);
    let id = column_at(E::ID).ok_or(AdminConfigError::UnsupportedId { entity })?;
    let version = E::VERSION.and_then(column_at);
    let create_id = !E::ID_GENERATED;
    let filled = |at: usize| {
      let column = fields[at].column;
      form.contains(&at)
        || at == id
        || Some(at) == version
        || Some(column) == E::TENANT
        || Some(column) == E::SOFT_DELETE
        || fields[at].nullable
    };
    let creatable = options.template.is_some() || (0..fields.len()).all(filled);
    let display = search
      .first()
      .and_then(|column| column_at(column))
      .or_else(|| {
        (0..fields.len()).find(|at| {
          *at != id && !excluded(*at) && ValueKind::of(fields[*at].schema_type) == ValueKind::Text
        })
      })
      .unwrap_or(id);
    let mut actions: Vec<ActionSpec> = Vec::with_capacity(options.actions.len());
    for action in &options.actions {
      let spec = action.spec();
      let valid = crate::permission::check_entity(&spec.name).is_ok()
        && spec.name != "delete"
        && !actions.iter().any(|a| a.name == spec.name);
      if !valid {
        return Err(AdminConfigError::InvalidAction {
          entity,
          action: spec.name,
        });
      }
      actions.push(spec);
    }
    Ok(Self {
      form,
      create_id,
      creatable,
      version,
      display,
      actions,
      relations: relations::specs::<E>(),
    })
  }
}

/// The typed half of an entity's editing configuration (closures over `E`).
pub(crate) struct Typed<E> {
  pub(crate) writer: Writer<E>,
  validate: Option<Validator<E>>,
  template: Option<Template<E>>,
  pub(crate) actions: Vec<BulkAction<E>>,
}

impl<E: Entity> Typed<E> {
  pub(crate) fn new(options: &EditOptions<E>) -> Self {
    Self {
      writer: options
        .writer
        .clone()
        .unwrap_or_else(|| writer(EntityHooks::new())),
      validate: options.validate.clone(),
      template: options.template.clone(),
      actions: options.actions.clone(),
    }
  }
}

/// One admin route: method, path, handler name, method router.
pub(crate) struct AdminRoute {
  pub(crate) method: Method,
  pub(crate) path: String,
  pub(crate) handler: &'static str,
  pub(crate) router: MethodRouter,
}

impl AdminRoute {
  pub(crate) fn new(
    method: Method,
    path: String,
    handler: &'static str,
    router: MethodRouter,
  ) -> Self {
    Self {
      method,
      path,
      handler,
      router,
    }
  }
}

/// `<prefix>/<entity>/@new`.
pub(crate) fn new_path(site: &SiteShared, slug: &str) -> String {
  format!("{}/@new", site.list_path(slug))
}

/// `<prefix>/<entity>/<id>/@edit`.
pub(crate) fn edit_path(site: &SiteShared, slug: &str, id: &str) -> String {
  format!("{}/@edit", site.detail_path(slug, id))
}

/// `<prefix>/<entity>/<id>/@delete`.
pub(crate) fn delete_path(site: &SiteShared, slug: &str, id: &str) -> String {
  format!("{}/@delete", site.detail_path(slug, id))
}

/// The create, edit and delete routes of `E`.
pub(crate) fn routes<E: Entity>(
  site: &Arc<SiteShared>,
  spec: &Arc<EntitySpec>,
  typed: &Arc<Typed<E>>,
) -> Vec<AdminRoute> {
  let list = site.list_path(&spec.slug);
  let with = || (Arc::clone(site), Arc::clone(spec), Arc::clone(typed));
  let (s1, p1, t1) = with();
  let (s2, p2, t2) = with();
  let (s3, p3, t3) = with();
  let (s4, p4, t4) = with();
  let (s5, p5) = (Arc::clone(site), Arc::clone(spec));
  let (s6, p6, t6) = with();
  vec![
    AdminRoute::new(
      Method::GET,
      format!("{list}/@new"),
      "id_effect_admin::new",
      axum::routing::get(move |req: Request| {
        new_form::<E>(Arc::clone(&s1), Arc::clone(&p1), Arc::clone(&t1), req)
      }),
    ),
    AdminRoute::new(
      Method::POST,
      format!("{list}/@new"),
      "id_effect_admin::create",
      axum::routing::post(move |req: Request| {
        create::<E>(Arc::clone(&s2), Arc::clone(&p2), Arc::clone(&t2), req)
      }),
    ),
    AdminRoute::new(
      Method::GET,
      format!("{list}/{{id}}/@edit"),
      "id_effect_admin::edit",
      axum::routing::get(move |Path(id): Path<String>, req: Request| {
        edit_form::<E>(Arc::clone(&s3), Arc::clone(&p3), Arc::clone(&t3), id, req)
      }),
    ),
    AdminRoute::new(
      Method::POST,
      format!("{list}/{{id}}/@edit"),
      "id_effect_admin::update",
      axum::routing::post(move |Path(id): Path<String>, req: Request| {
        update::<E>(Arc::clone(&s4), Arc::clone(&p4), Arc::clone(&t4), id, req)
      }),
    ),
    AdminRoute::new(
      Method::GET,
      format!("{list}/{{id}}/@delete"),
      "id_effect_admin::confirm_delete",
      axum::routing::get(move |Path(id): Path<String>, req: Request| {
        confirm_delete::<E>(Arc::clone(&s5), Arc::clone(&p5), id, req)
      }),
    ),
    AdminRoute::new(
      Method::POST,
      format!("{list}/{{id}}/@delete"),
      "id_effect_admin::delete",
      axum::routing::post(move |Path(id): Path<String>, req: Request| {
        delete::<E>(Arc::clone(&s6), Arc::clone(&p6), Arc::clone(&t6), id, req)
      }),
    ),
  ]
}

// ---- shared steps ----

/// The CSRF token the `SecurityLayer` put on the request, if it ran.
pub(crate) fn csrf_token(ctx: &Ctx) -> Option<CsrfToken> {
  ctx.parts.extensions.get::<CsrfToken>().cloned()
}

/// The hidden CSRF field of a form (nothing without the layer: the post is refused anyway).
pub(crate) fn csrf_input(ctx: &Ctx) -> Markup {
  html! { @if let Some(token) = csrf_token(ctx) { (csrf_field(&token)) } }
}

/// The checks of every admin write before its body is read: the permission(s), then the CSRF
/// check of the `SecurityLayer` (a request it did not check is refused).
pub(crate) async fn authorize_write(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  actions: &[AdminAction],
) -> Result<(), Box<Response>> {
  for action in actions {
    ctx.require(site, spec, *action).await?;
  }
  if csrf_token(ctx).is_none() {
    tracing::warn!(
      target: "id_effect_admin",
      entity = spec.entity,
      "admin write refused: the request did not pass a CSRF check (install SecurityLayer)"
    );
    return Err(Box::new(ctx.error(
      site,
      StatusCode::FORBIDDEN,
      "admin.csrf",
    )));
  }
  Ok(())
}

/// The bounded form body of a write (413 over 64 KiB, 415 not a form, 400 malformed).
pub(crate) async fn read_body(
  ctx: &Ctx,
  site: &SiteShared,
  body: Body,
) -> Result<FormBody, Box<Response>> {
  forms::read_form(&ctx.parts.headers, body)
    .await
    .map_err(|status| {
      let key = if status == StatusCode::PAYLOAD_TOO_LARGE {
        "admin.too-large"
      } else {
        "admin.bad-request"
      };
      Box::new(ctx.error(site, status, key))
    })
}

/// Run `effect`, or the error page of its failure: 409 on a stale version or a duplicate key,
/// 404 when the row is gone, 500 otherwise (logged).
pub(crate) async fn run<A: Send + 'static>(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  effect: Effect<A, SqlError>,
) -> Result<A, Box<Response>> {
  match EffectExecutor::global().run_effect(effect).await {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(SqlError::StaleVersion { .. })) => Err(Box::new(ctx.error(
      site,
      StatusCode::CONFLICT,
      "admin.conflict",
    ))),
    Exit::Failure(Cause::Fail(SqlError::RowNotFound)) => Err(Box::new(ctx.error(
      site,
      StatusCode::NOT_FOUND,
      "admin.not-found",
    ))),
    Exit::Failure(Cause::Fail(e)) if e.sqlstate() == Some(SQLSTATE_UNIQUE_VIOLATION) => Err(
      Box::new(ctx.error(site, StatusCode::CONFLICT, "admin.duplicate")),
    ),
    Exit::Failure(cause) => {
      tracing::error!(target: "id_effect_admin", entity = spec.entity, ?cause, "admin statement failed");
      Err(Box::new(ctx.error(
        site,
        StatusCode::INTERNAL_SERVER_ERROR,
        "admin.error",
      )))
    }
  }
}

/// The row `id` (path text) of `E` in the request's tenant, or 404.
pub(crate) async fn load<E: Entity>(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  repo: &Repository<E>,
  id: &str,
) -> Result<E, Box<Response>> {
  let not_found = || Box::new(ctx.error(site, StatusCode::NOT_FOUND, "admin.not-found"));
  let id = spec.id_kind.parse(id).ok_or_else(not_found)?;
  let read = repo
    .on(site.db.client())
    .find_by(Expr::col(E::ID).eq(Value::from(id)));
  run(ctx, site, spec, read)
    .await?
    .into_iter()
    .next()
    .ok_or_else(not_found)
}

/// The value of `column` in `values` (NULL when absent).
pub(crate) fn value_of(values: &[(&'static str, SqlValue)], column: &str) -> SqlValue {
  values
    .iter()
    .find(|(c, _)| *c == column)
    .map_or(SqlValue::Null, |(_, v)| v.clone())
}

/// The primary key of `record` as path text.
pub(crate) fn id_of<E: Entity>(spec: &EntitySpec, record: &E) -> String {
  id_text(&value_of(&record.values(), spec.id_field().column))
}

/// The `(field name, value)` pairs of the form fields of `values` (what the audit records:
/// never an excluded or non-editable field).
fn form_values(
  spec: &EntitySpec,
  values: &[(&'static str, SqlValue)],
) -> Vec<(&'static str, SqlValue)> {
  spec
    .edit
    .form
    .iter()
    .map(|at| {
      (
        spec.fields[*at].name,
        value_of(values, spec.fields[*at].column),
      )
    })
    .collect()
}

/// Where a write lands: the record's detail page when the principal may view it, else the list.
async fn after_write(ctx: &Ctx, site: &SiteShared, spec: &EntitySpec, id: &str) -> Response {
  let target = if ctx.allowed(site, spec, AdminAction::View).await.is_ok() {
    site.detail_path(&spec.slug, id)
  } else {
    site.list_path(&spec.slug)
  };
  Redirect::to(&target).into_response()
}

/// The field (Rust name) an issue path names: its first segment as a column or a field name;
/// `""` (the form) when it names neither.
fn issue_field(spec: &EntitySpec, path: &str) -> &'static str {
  let head = path.split(['.', '/']).find(|s| !s.is_empty()).unwrap_or("");
  spec
    .fields
    .iter()
    .find(|f| f.column == head || f.name == head)
    .map_or("", |f| f.name)
}

/// Bind `message` to `field` — to the form when the form does not show that field.
fn bind_error(spec: &EntitySpec, state: FormState, field: &str, message: &str) -> FormState {
  let shown = spec
    .edit
    .form
    .iter()
    .any(|at| spec.fields[*at].name == field)
    || (spec.edit.create_id && spec.id_field().name == field);
  state.with_error(if shown { field } else { "" }, message)
}

/// Log the submitted names the form does not declare (names only, bounded).
fn log_ignored(spec: &EntitySpec, ignored: &[String]) {
  if ignored.is_empty() {
    return;
  }
  let names: Vec<String> = ignored
    .iter()
    .take(16)
    .map(|n| n.chars().take(64).collect())
    .collect();
  tracing::warn!(
    target: "id_effect_admin",
    entity = spec.entity,
    ignored = ?names,
    "admin form: fields that are not declared editable were ignored"
  );
}

/// The zero value of a field's kind (a placeholder the database or repository replaces).
fn zero(kind: ValueKind) -> SqlValue {
  match kind {
    ValueKind::Bool => SqlValue::Bool(false),
    ValueKind::I16 => SqlValue::I16(0),
    ValueKind::I32 => SqlValue::I32(0),
    ValueKind::I64 => SqlValue::I64(0),
    ValueKind::F32 => SqlValue::F32(0.0),
    ValueKind::F64 => SqlValue::F64(0.0),
    ValueKind::Text => SqlValue::Text(String::new()),
    ValueKind::Uuid => SqlValue::Uuid(uuid::Uuid::nil()),
    ValueKind::Date => SqlValue::Date(jiff::civil::date(1970, 1, 1)),
    ValueKind::Timestamp => SqlValue::Timestamp(jiff::Timestamp::UNIX_EPOCH),
    ValueKind::Other => SqlValue::Null,
  }
}

/// The values a new record starts from: the template's, else NULL or the zero of each column.
fn base_values<E: Entity>(spec: &EntitySpec, typed: &Typed<E>) -> Vec<(&'static str, SqlValue)> {
  if let Some(template) = &typed.template {
    return template().values();
  }
  spec
    .fields
    .iter()
    .map(|f| {
      let value = if f.nullable {
        SqlValue::Null
      } else {
        zero(ValueKind::of(f.schema_type))
      };
      (f.column, value)
    })
    .collect()
}

/// `values` with the bound form fields parsed into them; parse errors bound to `state`.
fn apply_form(
  spec: &EntitySpec,
  values: &mut [(&'static str, SqlValue)],
  body: &FormBody,
  create: bool,
  mut state: FormState,
) -> FormState {
  let with_id = create && spec.edit.create_id;
  let mut fields: Vec<&FieldMeta> = spec.edit.form.iter().map(|at| &spec.fields[*at]).collect();
  if with_id {
    fields.push(spec.id_field());
  }
  let names: Vec<&str> = fields.iter().map(|f| f.name).collect();
  let bound = body.bind(&names);
  log_ignored(spec, bound.ignored());
  for meta in fields {
    // A field the request leaves out: empty on a create, unchanged on an update.
    let text = match bound.value(meta.name) {
      Some(text) => text,
      None if create => "",
      None => continue,
    };
    let parsed = if with_id && meta.name == spec.id_field().name && text.trim().is_empty() {
      Err("must not be empty".to_owned())
    } else {
      forms::parse_field(meta, text)
    };
    match parsed {
      Ok(value) => {
        if let Some(slot) = values.iter_mut().find(|(c, _)| *c == meta.column) {
          slot.1 = value;
        }
      }
      Err(message) => state = bind_error(spec, state, meta.name, &message),
    }
  }
  state
}

/// The record decoded from `values` and validated, or `state` with every issue bound.
fn decode<E: Entity>(
  spec: &EntitySpec,
  typed: &Typed<E>,
  values: &[(&'static str, SqlValue)],
  mut state: FormState,
  invalid: &str,
) -> Result<(E, FormState), FormState> {
  let row = SqlRow::from_pairs(values.iter().map(|(c, v)| (*c, v.clone())));
  match E::from_row(&row) {
    Ok(record) => {
      if let Some(Err(errors)) = typed.validate.as_ref().map(|v| v(&record)) {
        for issue in &errors.issues {
          state = bind_error(spec, state, issue_field(spec, &issue.path), &issue.message);
        }
      }
      if state.is_valid() {
        Ok((record, state))
      } else {
        Err(state)
      }
    }
    Err(SqlError::InvalidRow { issues }) => {
      for issue in &issues {
        state = bind_error(spec, state, issue_field(spec, &issue.path), &issue.message);
      }
      Err(state)
    }
    Err(other) => {
      tracing::warn!(target: "id_effect_admin", entity = spec.entity, error = %other, "admin form: record does not decode");
      Err(state.with_error("", invalid))
    }
  }
}

/// A form's state from stored `values` (its form fields).
fn state_of(spec: &EntitySpec, values: &[(&'static str, SqlValue)]) -> FormState {
  spec.edit.form.iter().fold(FormState::new(), |state, at| {
    let f = &spec.fields[*at];
    state.with_value(f.name, forms::form_text(&value_of(values, f.column)))
  })
}

/// The record form: crumbs, title, errors, CSRF, the fields (pickers for `belongs_to` keys).
#[allow(clippy::too_many_arguments)]
async fn form_page(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  title: &str,
  trail: Vec<(String, Option<String>)>,
  action: &str,
  cancel: &str,
  state: FormState,
  version: Option<String>,
  with_id: bool,
  status: StatusCode,
) -> Response {
  let state = state.translate(|e| ctx.labels.schema_message(e.message()));
  let pickers: HashMap<usize, Markup> = relations::pickers(ctx, site, spec, &state).await;
  let (yes, no) = (ctx.labels.ui("admin.yes"), ctx.labels.ui("admin.no"));
  let submit = if with_id {
    ctx.labels.ui("admin.create")
  } else {
    ctx.labels.ui("admin.save")
  };
  let content = html! {
    section.admin-form {
      (ctx.crumbs(site, &trail))
      h1 { (title) }
      @if !state.is_valid() { p.admin-invalid role="alert" { (ctx.labels.ui("admin.invalid")) } }
      (state.error_summary())
      form method="post" action=(action) novalidate {
        (csrf_input(ctx))
        @if let Some(version) = &version { input type="hidden" name="_version" value=(version); }
        @if with_id && spec.edit.create_id {
          (forms::input(&state, spec.id_field(), &ctx.field_label(spec, spec.id), &yes, &no))
        }
        @for at in &spec.edit.form {
          @if let Some(picker) = pickers.get(at) {
            (picker)
          } @else {
            (forms::input(&state, &spec.fields[*at], &ctx.field_label(spec, *at), &yes, &no))
          }
        }
        div.admin-form-actions {
          button type="submit" { (submit) }
          " "
          a.admin-cancel href=(cancel) { (ctx.labels.ui("admin.cancel")) }
        }
      }
    }
  };
  ctx.page(site, title, content, status)
}

/// The "New" link of the list page, when the principal may create and the entity can be created.
pub(crate) async fn new_link(ctx: &Ctx, site: &SiteShared, spec: &EntitySpec) -> Markup {
  let allowed = spec.edit.creatable && ctx.allowed(site, spec, AdminAction::Create).await.is_ok();
  html! {
    @if allowed {
      p.admin-actions { a.admin-new href=(new_path(site, &spec.slug)) { (ctx.labels.ui("admin.new")) } }
    }
  }
}

/// The "Edit" and "Delete" links of a detail page, as the principal may.
pub(crate) async fn record_links(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  id: &str,
) -> Markup {
  let edit =
    !spec.edit.form.is_empty() && ctx.allowed(site, spec, AdminAction::Update).await.is_ok();
  let delete = ctx.allowed(site, spec, AdminAction::Delete).await.is_ok();
  html! {
    @if edit || delete {
      p.admin-actions {
        @if edit { a.admin-edit href=(edit_path(site, &spec.slug, id)) { (ctx.labels.ui("admin.edit")) } " " }
        @if delete { a.admin-delete href=(delete_path(site, &spec.slug, id)) { (ctx.labels.ui("admin.delete")) } }
      }
    }
  }
}

/// The breadcrumb trail of a record page: the list, then the record.
fn trail(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  id: Option<&str>,
  page: String,
) -> Vec<(String, Option<String>)> {
  let mut trail = vec![(ctx.entity_label(spec), Some(site.list_path(&spec.slug)))];
  if let Some(id) = id {
    trail.push((id.to_owned(), Some(site.detail_path(&spec.slug, id))));
  }
  trail.push((page, None));
  trail
}

// ---- handlers ----

/// `GET <prefix>/<entity>/@new`.
async fn new_form<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  typed: Arc<Typed<E>>,
  req: Request,
) -> Response {
  let (parts, _body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  if let Err(denied) = ctx.require(&site, &spec, AdminAction::Create).await {
    return *denied;
  }
  if !spec.edit.creatable {
    return ctx.error(&site, StatusCode::NOT_FOUND, "admin.not-found");
  }
  let state = match &typed.template {
    Some(template) => state_of(&spec, &template().values()),
    None => FormState::new(),
  };
  let title = format!(
    "{} — {}",
    ctx.labels.ui("admin.new"),
    ctx.entity_label(&spec)
  );
  let trail = trail(&ctx, &site, &spec, None, ctx.labels.ui("admin.new"));
  let action = new_path(&site, &spec.slug);
  let cancel = site.list_path(&spec.slug);
  form_page(
    &ctx,
    &site,
    &spec,
    &title,
    trail,
    &action,
    &cancel,
    state,
    None,
    true,
    StatusCode::OK,
  )
  .await
}

/// `POST <prefix>/<entity>/@new`.
async fn create<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  typed: Arc<Typed<E>>,
  req: Request,
) -> Response {
  let (parts, body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  match create_inner(&ctx, &site, &spec, &typed, body).await {
    Ok(response) => response,
    Err(response) => *response,
  }
}

async fn create_inner<E: Entity>(
  ctx: &Ctx,
  site: &Arc<SiteShared>,
  spec: &Arc<EntitySpec>,
  typed: &Arc<Typed<E>>,
  body: Body,
) -> Result<Response, Box<Response>> {
  authorize_write(ctx, site, spec, &[AdminAction::Create]).await?;
  if !spec.edit.creatable {
    return Err(Box::new(ctx.error(
      site,
      StatusCode::NOT_FOUND,
      "admin.not-found",
    )));
  }
  let repo = ctx.repository::<E>(site)?;
  let body = read_body(ctx, site, body).await?;
  let mut values = base_values(spec, typed);
  if let (Some(column), Some(tenant)) = (E::TENANT, repo.tenant())
    && let Some(slot) = values.iter_mut().find(|(c, _)| *c == column)
  {
    slot.1 = tenant.clone();
  }
  let state = FormState::from_pairs(body.pairs().iter().cloned());
  let state = apply_form(spec, &mut values, &body, true, state);
  let state = relations::check_references(ctx, site, spec, &values, state).await;
  let decoded = if state.is_valid() {
    decode(spec, typed, &values, state, &ctx.labels.ui("admin.invalid"))
  } else {
    Err(state)
  };
  let record = match decoded {
    Ok((record, _)) => record,
    Err(state) => {
      let title = format!(
        "{} — {}",
        ctx.labels.ui("admin.new"),
        ctx.entity_label(spec)
      );
      let trail = trail(ctx, site, spec, None, ctx.labels.ui("admin.new"));
      let (action, cancel) = (new_path(site, &spec.slug), site.list_path(&spec.slug));
      return Ok(
        form_page(
          ctx,
          site,
          spec,
          &title,
          trail,
          &action,
          &cancel,
          state,
          None,
          true,
          StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await,
      );
    }
  };
  let actor = Actor::of(ctx);
  let (writer, write_site, write_spec) = (typed.writer.clone(), Arc::clone(site), Arc::clone(spec));
  let write = with_transaction(site.db.client().0, move |tx| {
    writer.insert(&repo, &tx, record).flat_map(move |stored| {
      let values = stored.values();
      let change = Change::new(
        audit::ACTION_CREATE,
        &write_spec.slug,
        id_text(&value_of(&values, write_spec.id_field().column)),
      )
      .after(form_values(&write_spec, &values));
      audit::record(&write_site, &actor, &tx, change).map(move |()| stored)
    })
  });
  let stored = run(ctx, site, spec, write).await?;
  Ok(after_write(ctx, site, spec, &id_of(spec, &stored)).await)
}

/// `GET <prefix>/<entity>/{id}/@edit`.
async fn edit_form<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  _typed: Arc<Typed<E>>,
  id: String,
  req: Request,
) -> Response {
  let (parts, _body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  let page = async {
    ctx.require(&site, &spec, AdminAction::Update).await?;
    let repo = ctx.repository::<E>(&site)?;
    let record = load(&ctx, &site, &spec, &repo, &id).await?;
    let values = record.values();
    let id = id_of(&spec, &record);
    let version = spec
      .edit
      .version
      .map(|at| forms::form_text(&value_of(&values, spec.fields[at].column)));
    let title = format!(
      "{} — {} {id}",
      ctx.labels.ui("admin.edit"),
      ctx.entity_label(&spec)
    );
    let trail = trail(&ctx, &site, &spec, Some(&id), ctx.labels.ui("admin.edit"));
    let (action, cancel) = (
      edit_path(&site, &spec.slug, &id),
      site.detail_path(&spec.slug, &id),
    );
    let state = state_of(&spec, &values);
    Ok::<_, Box<Response>>(
      form_page(
        &ctx,
        &site,
        &spec,
        &title,
        trail,
        &action,
        &cancel,
        state,
        version,
        false,
        StatusCode::OK,
      )
      .await,
    )
  };
  page.await.unwrap_or_else(|r| *r)
}

/// `POST <prefix>/<entity>/{id}/@edit`.
async fn update<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  typed: Arc<Typed<E>>,
  id: String,
  req: Request,
) -> Response {
  let (parts, body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  match update_inner(&ctx, &site, &spec, &typed, &id, body).await {
    Ok(response) => response,
    Err(response) => *response,
  }
}

async fn update_inner<E: Entity>(
  ctx: &Ctx,
  site: &Arc<SiteShared>,
  spec: &Arc<EntitySpec>,
  typed: &Arc<Typed<E>>,
  id: &str,
  body: Body,
) -> Result<Response, Box<Response>> {
  authorize_write(ctx, site, spec, &[AdminAction::Update]).await?;
  let repo = ctx.repository::<E>(site)?;
  let body = read_body(ctx, site, body).await?;
  let current = load(ctx, site, spec, &repo, id).await?;
  let before = current.values();
  let mut values = before.clone();
  let id = id_of(spec, &current);
  let mut version = None;
  if let Some(at) = spec.edit.version
    && let Some(text) = body.value("_version")
  {
    let field = &spec.fields[at];
    let Ok(submitted) = forms::parse_field(field, text) else {
      return Err(Box::new(ctx.error(
        site,
        StatusCode::CONFLICT,
        "admin.conflict",
      )));
    };
    if let Some(slot) = values.iter_mut().find(|(c, _)| *c == field.column) {
      slot.1 = submitted;
    }
    version = Some(text.to_owned());
  }
  let state = FormState::from_pairs(body.pairs().iter().cloned());
  let state = apply_form(spec, &mut values, &body, false, state);
  let state = relations::check_references(ctx, site, spec, &values, state).await;
  let decoded = if state.is_valid() {
    decode(spec, typed, &values, state, &ctx.labels.ui("admin.invalid"))
  } else {
    Err(state)
  };
  let record = match decoded {
    Ok((record, _)) => record,
    Err(state) => {
      let title = format!(
        "{} — {} {id}",
        ctx.labels.ui("admin.edit"),
        ctx.entity_label(spec)
      );
      let trail = trail(ctx, site, spec, Some(&id), ctx.labels.ui("admin.edit"));
      let (action, cancel) = (
        edit_path(site, &spec.slug, &id),
        site.detail_path(&spec.slug, &id),
      );
      return Ok(
        form_page(
          ctx,
          site,
          spec,
          &title,
          trail,
          &action,
          &cancel,
          state,
          version,
          false,
          StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await,
      );
    }
  };
  let old = form_values(spec, &before);
  let new = form_values(spec, &record.values());
  let changed: Vec<usize> = (0..old.len()).filter(|i| old[*i] != new[*i]).collect();
  if changed.is_empty() {
    return Ok(after_write(ctx, site, spec, &id).await);
  }
  let pick =
    |pairs: &[(&'static str, SqlValue)]| changed.iter().map(|i| pairs[*i].clone()).collect();
  let change = Change::new(audit::ACTION_UPDATE, &spec.slug, id.clone())
    .before(pick(&old))
    .after(pick(&new));
  let (actor, writer, audit_site) = (Actor::of(ctx), typed.writer.clone(), Arc::clone(site));
  let write = with_transaction(site.db.client().0, move |tx| {
    writer
      .update(&repo, &tx, record)
      .flat_map(move |_| audit::record(&audit_site, &actor, &tx, change))
  });
  run(ctx, site, spec, write).await?;
  Ok(after_write(ctx, site, spec, &id).await)
}

/// `GET <prefix>/<entity>/{id}/@delete`: the confirmation.
async fn confirm_delete<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  id: String,
  req: Request,
) -> Response {
  let (parts, _body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  let page = async {
    ctx.require(&site, &spec, AdminAction::Delete).await?;
    let repo = ctx.repository::<E>(&site)?;
    let record = load(&ctx, &site, &spec, &repo, &id).await?;
    let id = id_of(&spec, &record);
    let title = format!(
      "{} — {} {id}",
      ctx.labels.ui("admin.delete"),
      ctx.entity_label(&spec)
    );
    let trail = trail(&ctx, &site, &spec, Some(&id), ctx.labels.ui("admin.delete"));
    let content = html! {
      section.admin-confirm {
        (ctx.crumbs(&site, &trail))
        h1 { (title) }
        p { (ctx.labels.ui("admin.delete-confirm")) }
        form method="post" action=(delete_path(&site, &spec.slug, &id)) {
          (csrf_input(&ctx))
          button.admin-danger type="submit" { (ctx.labels.ui("admin.delete")) }
          " "
          a.admin-cancel href=(site.detail_path(&spec.slug, &id)) { (ctx.labels.ui("admin.cancel")) }
        }
      }
    };
    Ok::<_, Box<Response>>(ctx.page(&site, &title, content, StatusCode::OK))
  };
  page.await.unwrap_or_else(|r| *r)
}

/// `POST <prefix>/<entity>/{id}/@delete`.
async fn delete<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  typed: Arc<Typed<E>>,
  id: String,
  req: Request,
) -> Response {
  let (parts, body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  let done = async {
    authorize_write(&ctx, &site, &spec, &[AdminAction::Delete]).await?;
    let repo = ctx.repository::<E>(&site)?;
    read_body(&ctx, &site, body).await?;
    let record = load(&ctx, &site, &spec, &repo, &id).await?;
    let id = id_of(&spec, &record);
    let change = Change::new(audit::ACTION_DELETE, &spec.slug, id)
      .before(form_values(&spec, &record.values()));
    let (actor, writer, audit_site) = (Actor::of(&ctx), typed.writer.clone(), Arc::clone(&site));
    let write = with_transaction(site.db.client().0, move |tx| {
      writer.delete(&repo, &tx, record).flat_map(move |deleted| {
        audit::record(&audit_site, &actor, &tx, change).map(move |()| deleted)
      })
    });
    run(&ctx, &site, &spec, write).await?;
    Ok::<_, Box<Response>>(Redirect::to(&site.list_path(&spec.slug)).into_response())
  };
  done.await.unwrap_or_else(|r| *r)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn zeros_cover_every_kind() {
    use ValueKind::*;
    let zeros: Vec<SqlValue> = [
      Bool, I16, I32, I64, F32, F64, Text, Uuid, Date, Timestamp, Other,
    ]
    .into_iter()
    .map(zero)
    .collect();
    assert_eq!(zeros[0], SqlValue::Bool(false));
    assert_eq!(zeros[3], SqlValue::I64(0));
    assert_eq!(zeros[10], SqlValue::Null);
    assert!(matches!(zeros[9], SqlValue::Timestamp(_)));
  }
}
