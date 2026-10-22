//! Relations in the admin (web.admin #4), from the entity's declared relations
//! ([`Entity::RELATIONS`]): a picker for each `belongs_to` foreign key on the forms, and an
//! inline list for each `has_many` and `many_to_many` relation on the detail page, with add and
//! remove for the join-table (`many_to_many`) ones.
//!
//! A relation shows only when its target entity is registered on the same site and the
//! principal may list it (`admin.<target>.list`); otherwise the foreign key is a plain input and
//! the inline list is left out.
//!
//! | Route | What | Permission |
//! |---|---|---|
//! | `GET <prefix>/<entity>/@pick/<relation>?_q=` | the picker's `<option>`s: target rows matching the text | `create` or `update` on the entity, `list` on the target |
//! | `POST <prefix>/<entity>/{id}/@rel/<relation>` | `op=add` / `op=remove` a `target` (join-table relations) | `update` on the entity, `list` on the target |
//!
//! **Tenant-scoped** (ADR 0013 R1): every target row — picker choices, inline lists, the check
//! of a submitted foreign key, the target of an add — is read through the target entity's
//! repository bound to the request's tenant, so a picker never lists another tenant's rows, a
//! foreign key naming one is refused on the form ("must reference an existing record"), and an
//! add naming one is refused (400). Join-table writes go through the request's transaction with
//! their audit record (feature `audit`) and need the CSRF check like every admin write.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::{Path, Request};
use axum::http::{Method, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use id_effect::{Effect, Exit, succeed};
use id_effect_axum::EffectExecutor;
use id_effect_axum::ui::form::FormState;
use id_effect_axum::ui::{Markup, html};
use id_effect_sql::query_builder::BuildSql;
use id_effect_sql::query_builder::sea_query::{
  Condition, Expr, ExprTrait, Func, LikeExpr, Query as Sql, Value,
};
use id_effect_sql::repository::RelationKind;
use id_effect_sql::{Entity, Page, SqlError, SqlValue, with_transaction};

use crate::audit::{self, Actor, Change};
use crate::edit::{AdminRoute, authorize_write, csrf_input, load, read_body, value_of};
use crate::entity::EntitySpec;
use crate::list::{MAX_QUERY_LEN, MAX_SEARCH_CHARS, escape_like};
use crate::permission::AdminAction;
use crate::site::{Ctx, SiteShared};
use crate::value::{ValueKind, display, humanize, id_text, truncate};

/// The most rows a picker offers.
pub const MAX_CHOICES: u64 = 20;
/// The most rows an inline list shows.
pub const MAX_INLINE: u64 = 100;
/// The longest label of a choice, in characters.
const LABEL_CHARS: usize = 80;
/// The message bound to a foreign key naming no row of the target in the tenant.
pub const NOT_A_REFERENCE: &str = "must reference an existing record";

/// A declared relation, checked.
#[derive(Clone, Debug)]
pub(crate) struct RelationSpec {
  pub(crate) name: &'static str,
  pub(crate) kind: RelationKind,
  pub(crate) target: &'static str,
  /// The foreign-key field of a `belongs_to` relation.
  pub(crate) field: Option<usize>,
}

/// The relations of `E`.
pub(crate) fn specs<E: Entity>() -> Vec<RelationSpec> {
  E::RELATIONS
    .iter()
    .map(|r| RelationSpec {
      name: r.name,
      kind: r.kind,
      target: r.target,
      field: match r.kind {
        RelationKind::BelongsTo { foreign_key } => {
          E::FIELDS.iter().position(|f| f.column == foreign_key)
        }
        _ => None,
      },
    })
    .collect()
}

/// Which target rows to read.
pub(crate) enum Select {
  /// Rows whose search fields (else display field) contain the text.
  Search(String),
  /// Rows with these primary keys.
  Ids(Vec<SqlValue>),
  /// Rows whose `foreign_key` is `parent` (`has_many`).
  Children {
    foreign_key: &'static str,
    parent: SqlValue,
  },
  /// Rows paired with `parent` in the join table (`many_to_many`).
  Joined {
    through: &'static str,
    local_key: &'static str,
    foreign_key: &'static str,
    parent: SqlValue,
  },
}

/// A target row as a choice: its id (path text) and label.
#[derive(Clone, Debug)]
pub(crate) struct Choice {
  pub(crate) id: String,
  pub(crate) label: String,
}

/// A registered entity, read without its type: the target side of relations.
pub(crate) trait Lookup: Send + Sync {
  /// The entity's `Entity::NAME`.
  fn entity(&self) -> &'static str;
  /// Its configuration.
  fn spec(&self) -> &Arc<EntitySpec>;
  /// The rows `select` names, in the request's tenant, at most `limit`.
  fn select(
    &self,
    ctx: &Ctx,
    site: &SiteShared,
    select: Select,
    limit: u64,
  ) -> Result<Effect<Vec<Choice>, SqlError>, Box<Response>>;
}

/// The [`Lookup`] of `E`.
pub(crate) struct Reader<E> {
  spec: Arc<EntitySpec>,
  entity: PhantomData<fn() -> E>,
}

impl<E> Reader<E> {
  pub(crate) fn new(spec: Arc<EntitySpec>) -> Self {
    Self {
      spec,
      entity: PhantomData,
    }
  }
}

/// Rows of `spec` whose search fields (else its display field, when text) contain `q`.
fn search(spec: &EntitySpec, q: &str) -> Condition {
  let q: String = q.trim().chars().take(MAX_SEARCH_CHARS).collect();
  let display = spec.fields[spec.edit.display];
  let columns: Vec<&str> = if !spec.search.is_empty() {
    spec.search.clone()
  } else if ValueKind::of(display.schema_type) == ValueKind::Text {
    vec![display.column]
  } else {
    Vec::new()
  };
  if q.is_empty() || columns.is_empty() {
    return Condition::all();
  }
  let pattern = format!("%{}%", escape_like(&q.to_lowercase()));
  let any = columns.iter().fold(Condition::any(), |any, column| {
    any.add(
      Expr::expr(Func::lower(Expr::col(*column))).like(LikeExpr::new(pattern.clone()).escape('\\')),
    )
  });
  Condition::all().add(any)
}

impl<E: Entity> Lookup for Reader<E> {
  fn entity(&self) -> &'static str {
    E::NAME
  }

  fn spec(&self) -> &Arc<EntitySpec> {
    &self.spec
  }

  fn select(
    &self,
    ctx: &Ctx,
    site: &SiteShared,
    select: Select,
    limit: u64,
  ) -> Result<Effect<Vec<Choice>, SqlError>, Box<Response>> {
    let repo = ctx.repository::<E>(site)?;
    let cond = match select {
      Select::Search(q) => search(&self.spec, &q),
      Select::Ids(ids) => {
        Condition::all().add(Expr::col(E::ID).is_in(ids.into_iter().map(Value::from)))
      }
      Select::Children {
        foreign_key,
        parent,
      } => Condition::all().add(Expr::col(foreign_key).eq(Value::from(parent))),
      Select::Joined {
        through,
        local_key,
        foreign_key,
        parent,
      } => Condition::all().add(
        Expr::col((E::TABLE, E::ID)).in_subquery(
          Sql::select()
            .column(foreign_key)
            .from(through)
            .and_where(Expr::col(local_key).eq(Value::from(parent)))
            .to_owned(),
        ),
      ),
    };
    let spec = Arc::clone(&self.spec);
    let read = repo
      .on(site.db.client())
      .find_page(cond, Page::new(limit, 0));
    Ok(read.map(move |rows| {
      rows
        .iter()
        .map(|row| choice(&spec, &row.values()))
        .collect()
    }))
  }
}

/// A row of `spec` as a choice.
fn choice(spec: &EntitySpec, values: &[(&'static str, SqlValue)]) -> Choice {
  let id = id_text(&value_of(values, spec.id_field().column));
  let label = display(
    &value_of(values, spec.fields[spec.edit.display].column),
    "true",
    "false",
  );
  Choice {
    label: if label.is_empty() {
      id.clone()
    } else {
      truncate(label, LABEL_CHARS)
    },
    id,
  }
}

/// Run a lookup's `select`; `None` when it is refused or fails (logged).
async fn choices(
  ctx: &Ctx,
  site: &SiteShared,
  lookup: &dyn Lookup,
  select: Select,
  limit: u64,
) -> Option<Vec<Choice>> {
  let read = lookup.select(ctx, site, select, limit).ok()?;
  match EffectExecutor::global().run_effect(read).await {
    Exit::Success(choices) => Some(choices),
    Exit::Failure(cause) => {
      tracing::error!(target: "id_effect_admin", entity = lookup.entity(), ?cause, "admin relation read failed");
      None
    }
  }
}

/// The lookup of the relation's target, when it is registered and the principal may list it.
async fn target<'a>(
  ctx: &Ctx,
  site: &'a SiteShared,
  relation: &RelationSpec,
) -> Option<&'a Arc<dyn Lookup>> {
  let lookup = site.lookup(relation.target)?;
  ctx
    .allowed(site, lookup.spec(), AdminAction::List)
    .await
    .ok()
    .map(|()| lookup)
}

/// `<prefix>/<entity>/@pick/<relation>`.
fn pick_path(site: &SiteShared, slug: &str, relation: &str) -> String {
  format!("{}/@pick/{relation}", site.list_path(slug))
}

/// `<prefix>/<entity>/<id>/@rel/<relation>`.
fn relation_path(site: &SiteShared, slug: &str, id: &str, relation: &str) -> String {
  format!("{}/@rel/{relation}", site.detail_path(slug, id))
}

/// The `<option>`s of `choices`, `current` selected, with an empty first option when `empty`.
fn options(choices: &[Choice], current: &str, empty: bool) -> Markup {
  html! {
    @if empty { option value="" selected[current.is_empty()] { "—" } }
    @for c in choices { option value=(c.id) selected[c.id == current] { (c.label) } }
  }
}

/// The search box that refreshes the select `#select` from the pick route.
fn search_box(ctx: &Ctx, pick: &str, select: &str) -> Markup {
  let search = ctx.labels.ui("admin.find");
  html! {
    input.admin-pick type="search" name="_q" placeholder=(search) aria-label=(search)
      hx-get=(pick) hx-trigger="input changed delay:300ms, search" hx-target=(format!("#{select}"))
      hx-swap="innerHTML";
  }
}

/// The pickers of the form fields that are `belongs_to` foreign keys, by field index.
pub(crate) async fn pickers(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  state: &FormState,
) -> HashMap<usize, Markup> {
  let mut pickers = HashMap::new();
  for relation in &spec.edit.relations {
    let Some(at) = relation.field.filter(|at| spec.edit.form.contains(at)) else {
      continue;
    };
    let Some(lookup) = target(ctx, site, relation).await else {
      continue;
    };
    let meta = &spec.fields[at];
    let current = state.value(meta.name);
    let Some(mut found) = choices(
      ctx,
      site,
      &**lookup,
      Select::Search(String::new()),
      MAX_CHOICES,
    )
    .await
    else {
      continue;
    };
    if !current.is_empty()
      && !found.iter().any(|c| c.id == current)
      && let Some(id) = lookup.spec().id_kind.parse(current)
      && let Some(chosen) = choices(ctx, site, &**lookup, Select::Ids(vec![id]), 1).await
    {
      found.extend(chosen);
    }
    let label = ctx.field_label(spec, at);
    let id = state.field(meta.name, &label).id();
    let errors = state.errors(meta.name);
    let invalid = !errors.is_empty();
    let pick = pick_path(site, &spec.slug, relation.name);
    pickers.insert(
      at,
      html! {
        div.field.admin-picker.invalid[invalid] data-field=(meta.name) {
          label for=(id) { (label) }
          (search_box(ctx, &pick, &id))
          select id=(id) name=(meta.name) aria-invalid=[invalid.then_some("true")] {
            (options(&found, current, meta.nullable))
          }
          @if invalid {
            ul.field-errors { @for e in errors { li data-message-key=(e.key()) { (e.message()) } } }
          }
        }
      },
    );
  }
  pickers
}

/// `state` with an error on every `belongs_to` foreign key of the form whose value names no row
/// of the (registered) target in the request's tenant.
pub(crate) async fn check_references(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  values: &[(&'static str, SqlValue)],
  mut state: FormState,
) -> FormState {
  for relation in &spec.edit.relations {
    let Some(at) = relation.field.filter(|at| spec.edit.form.contains(at)) else {
      continue;
    };
    let Some(lookup) = site.lookup(relation.target) else {
      continue;
    };
    let meta = &spec.fields[at];
    let value = value_of(values, meta.column);
    if value == SqlValue::Null || !state.errors(meta.name).is_empty() {
      continue;
    }
    let found = choices(ctx, site, &**lookup, Select::Ids(vec![value]), 1).await;
    if found.is_none_or(|rows| rows.is_empty()) {
      state = state.with_error(meta.name, NOT_A_REFERENCE);
    }
  }
  state
}

/// The inline lists of the record `parent` (path text `id`) on its detail page.
pub(crate) async fn inline(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  parent: &SqlValue,
  id: &str,
) -> Markup {
  let can_update = ctx.allowed(site, spec, AdminAction::Update).await.is_ok();
  let mut sections = Vec::new();
  for relation in &spec.edit.relations {
    let select = match relation.kind {
      RelationKind::HasMany { foreign_key } => Select::Children {
        foreign_key,
        parent: parent.clone(),
      },
      RelationKind::ManyToMany {
        through,
        local_key,
        foreign_key,
      } => Select::Joined {
        through,
        local_key,
        foreign_key,
        parent: parent.clone(),
      },
      _ => continue,
    };
    let Some(lookup) = target(ctx, site, relation).await else {
      continue;
    };
    let Some(rows) = choices(ctx, site, &**lookup, select, MAX_INLINE).await else {
      continue;
    };
    let target_spec = lookup.spec();
    let view = ctx
      .allowed(site, target_spec, AdminAction::View)
      .await
      .is_ok();
    let joined = matches!(relation.kind, RelationKind::ManyToMany { .. });
    let editable = joined && can_update;
    let addable = if editable {
      choices(
        ctx,
        site,
        &**lookup,
        Select::Search(String::new()),
        MAX_CHOICES,
      )
      .await
      .unwrap_or_default()
    } else {
      Vec::new()
    };
    let title = ctx
      .labels
      .label(&format!("{}.{}", spec.label, relation.name), || {
        humanize(relation.name)
      });
    let action = relation_path(site, &spec.slug, id, relation.name);
    let select_id = format!("admin-add-{}", relation.name);
    sections.push(html! {
      section.admin-relation data-relation=(relation.name) {
        h2 { (title) }
        @if rows.is_empty() {
          p.admin-empty { (ctx.labels.ui("admin.none")) }
        } @else {
          ul {
            @for row in &rows {
              li {
                @if view { a href=(site.detail_path(&target_spec.slug, &row.id)) { (row.label) } } @else { (row.label) }
                @if editable {
                  " "
                  form.admin-inline method="post" action=(action) {
                    (csrf_input(ctx))
                    input type="hidden" name="op" value="remove";
                    input type="hidden" name="target" value=(row.id);
                    button type="submit" { (ctx.labels.ui("admin.remove")) }
                  }
                }
              }
            }
          }
        }
        @if editable {
          form.admin-inline-add method="post" action=(action) {
            (csrf_input(ctx))
            input type="hidden" name="op" value="add";
            (search_box(ctx, &pick_path(site, &spec.slug, relation.name), &select_id))
            select id=(select_id) name="target" aria-label=(title) { (options(&addable, "", false)) }
            " "
            button type="submit" { (ctx.labels.ui("admin.add")) }
          }
        }
      }
    });
  }
  html! { @for section in &sections { (section) } }
}

/// The relation routes of `E`: the pickers and the join-table add / remove.
pub(crate) fn routes<E: Entity>(site: &Arc<SiteShared>, spec: &Arc<EntitySpec>) -> Vec<AdminRoute> {
  let list = site.list_path(&spec.slug);
  let (s1, p1) = (Arc::clone(site), Arc::clone(spec));
  let (s2, p2) = (Arc::clone(site), Arc::clone(spec));
  vec![
    AdminRoute::new(
      Method::GET,
      format!("{list}/@pick/{{relation}}"),
      "id_effect_admin::pick",
      axum::routing::get(move |Path(relation): Path<String>, req: Request| {
        pick(Arc::clone(&s1), Arc::clone(&p1), relation, req)
      }),
    ),
    AdminRoute::new(
      Method::POST,
      format!("{list}/{{id}}/@rel/{{relation}}"),
      "id_effect_admin::relate",
      axum::routing::post(
        move |Path((id, relation)): Path<(String, String)>, req: Request| {
          relate::<E>(Arc::clone(&s2), Arc::clone(&p2), id, relation, req)
        },
      ),
    ),
  ]
}

/// `GET <prefix>/<entity>/@pick/<relation>?_q=`: the picker's options.
async fn pick(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  relation: String,
  req: Request,
) -> Response {
  let (parts, _body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  let answer = async {
    let not_found = || Box::new(ctx.error(&site, StatusCode::NOT_FOUND, "admin.not-found"));
    let relation = spec
      .edit
      .relations
      .iter()
      .find(|r| r.name == relation)
      .ok_or_else(not_found)?;
    let lookup = site.lookup(relation.target).ok_or_else(not_found)?;
    if ctx
      .allowed(&site, &spec, AdminAction::Create)
      .await
      .is_err()
    {
      ctx.require(&site, &spec, AdminAction::Update).await?;
    }
    ctx.require(&site, lookup.spec(), AdminAction::List).await?;
    let uri = &ctx.parts.uri;
    if uri.query().is_some_and(|q| q.len() > MAX_QUERY_LEN) {
      return Err(Box::new(ctx.error(
        &site,
        StatusCode::URI_TOO_LONG,
        "admin.bad-request",
      )));
    }
    let query = crate::forms::decode_form(uri.query().unwrap_or_default().as_bytes())
      .map_err(|_| Box::new(ctx.error(&site, StatusCode::BAD_REQUEST, "admin.bad-request")))?;
    let q = query.value("_q").unwrap_or_default().to_owned();
    let read = lookup.select(&ctx, &site, Select::Search(q), MAX_CHOICES)?;
    let found = crate::edit::run(&ctx, &site, lookup.spec(), read).await?;
    let empty = relation.field.is_some_and(|at| spec.fields[at].nullable);
    Ok::<_, Box<Response>>(Html(options(&found, "", empty).into_string()).into_response())
  };
  answer.await.unwrap_or_else(|r| *r)
}

/// `POST <prefix>/<entity>/{id}/@rel/<relation>`: add or remove a join-table pair.
async fn relate<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  id: String,
  relation: String,
  req: Request,
) -> Response {
  let (parts, body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  let done = async {
    let not_found = || Box::new(ctx.error(&site, StatusCode::NOT_FOUND, "admin.not-found"));
    let bad = || Box::new(ctx.error(&site, StatusCode::BAD_REQUEST, "admin.bad-request"));
    let relation = spec
      .edit
      .relations
      .iter()
      .find(|r| r.name == relation)
      .ok_or_else(not_found)?;
    let RelationKind::ManyToMany {
      through,
      local_key,
      foreign_key,
    } = relation.kind
    else {
      return Err(not_found());
    };
    let lookup = site.lookup(relation.target).ok_or_else(not_found)?;
    authorize_write(&ctx, &site, &spec, &[AdminAction::Update]).await?;
    ctx.require(&site, lookup.spec(), AdminAction::List).await?;
    let repo = ctx.repository::<E>(&site)?;
    let body = read_body(&ctx, &site, body).await?;
    let parent = load(&ctx, &site, &spec, &repo, &id).await?;
    let add = match body.value("op") {
      Some("add") => true,
      Some("remove") => false,
      _ => return Err(bad()),
    };
    let target = body
      .value("target")
      .and_then(|t| lookup.spec().id_kind.parse(t))
      .ok_or_else(bad)?;
    let found = choices(&ctx, &site, &**lookup, Select::Ids(vec![target.clone()]), 1).await;
    let Some(chosen) = found.and_then(|rows| rows.into_iter().next()) else {
      return Err(bad());
    };
    let parent_id = value_of(&parent.values(), E::ID);
    let id = id_text(&parent_id);
    let pair = || {
      Condition::all()
        .add(Expr::col(local_key).eq(Value::from(parent_id.clone())))
        .add(Expr::col(foreign_key).eq(Value::from(target.clone())))
    };
    let dialect = site.db.dialect();
    let statements = (|| {
      let exists = Sql::select()
        .expr(Expr::cust("1"))
        .from(through)
        .cond_where(pair())
        .limit(1)
        .to_owned()
        .build_sql(dialect)?;
      let insert = Sql::insert()
        .into_table(through)
        .columns([local_key, foreign_key])
        .values_panic([
          Expr::val(Value::from(parent_id.clone())),
          Expr::val(Value::from(target.clone())),
        ])
        .to_owned()
        .build_sql(dialect)?;
      let delete = Sql::delete()
        .from_table(through)
        .cond_where(pair())
        .to_owned()
        .build_sql(dialect)?;
      Ok::<_, SqlError>((exists, insert, delete))
    })();
    let ((exists, exists_params), (insert, insert_params), (delete, delete_params)) =
      statements.map_err(|error| {
        tracing::error!(target: "id_effect_admin", entity = spec.entity, %error, "admin relation statement");
        Box::new(ctx.error(&site, StatusCode::INTERNAL_SERVER_ERROR, "admin.error"))
      })?;
    let action = if add {
      audit::ACTION_RELATION_ADD
    } else {
      audit::ACTION_RELATION_REMOVE
    };
    let change = Change::new(action, &spec.slug, id.clone())
      .after(vec![(relation.name, SqlValue::Text(chosen.id))]);
    let (actor, audit_site) = (Actor::of(&ctx), Arc::clone(&site));
    let write = with_transaction(site.db.client().0, move |tx| {
      let changed = if add {
        let insert_tx = Arc::clone(&tx);
        tx.query(&exists, &exists_params).flat_map(move |rows| {
          if rows.is_empty() {
            insert_tx.execute(&insert, &insert_params)
          } else {
            succeed(0)
          }
        })
      } else {
        tx.execute(&delete, &delete_params)
      };
      changed.flat_map(move |n| {
        if n > 0 {
          audit::record(&audit_site, &actor, &tx, change)
        } else {
          succeed(())
        }
      })
    });
    crate::edit::run(&ctx, &site, &spec, write).await?;
    Ok::<_, Box<Response>>(Redirect::to(&site.detail_path(&spec.slug, &id)).into_response())
  };
  done.await.unwrap_or_else(|r| *r)
}
