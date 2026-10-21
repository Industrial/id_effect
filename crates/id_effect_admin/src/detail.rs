//! The detail page (web.admin #1 read half): one record's fields, from [`Entity::FIELDS`], read
//! through the request's tenant-bound repository.
//!
//! The id in the path is parsed as the primary key's type ([`FieldMeta::schema_type`]
//! (id_effect_sql::FieldMeta)); an id that does not parse, a missing row and a row of another
//! tenant all answer the same 404.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::Response;
use id_effect::Exit;
use id_effect_axum::EffectExecutor;
use id_effect_axum::ui::html;
use id_effect_sql::query_builder::sea_query::{Expr, ExprTrait, Value};
use id_effect_sql::{Entity, SqlValue};

use crate::entity::EntitySpec;
use crate::permission::AdminAction;
use crate::site::{Ctx, SiteShared};
use crate::value::{display, id_text};

/// `GET <prefix>/<entity>/{id}`.
pub(crate) async fn detail<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  id: String,
  req: Request,
) -> Response {
  let (parts, _body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  if let Err(denied) = ctx.require(&site, &spec, AdminAction::View).await {
    return *denied;
  }
  let repo = match ctx.repository::<E>(&site) {
    Ok(repo) => repo,
    Err(refused) => return *refused,
  };
  let Some(id) = spec.id_kind.parse(&id) else {
    return ctx.error(&site, StatusCode::NOT_FOUND, "admin.not-found");
  };
  let read = repo
    .on(site.db.client())
    .find_by(Expr::col(E::ID).eq(Value::from(id)));
  let row = match EffectExecutor::global().run_effect(read).await {
    Exit::Success(rows) => rows.into_iter().next(),
    Exit::Failure(cause) => {
      tracing::error!(target: "id_effect_admin", entity = spec.entity, ?cause, "admin detail failed");
      return ctx.error(&site, StatusCode::INTERNAL_SERVER_ERROR, "admin.error");
    }
  };
  let Some(row) = row else {
    return ctx.error(&site, StatusCode::NOT_FOUND, "admin.not-found");
  };
  let values = row.values();
  let value = |column: &str| {
    values
      .iter()
      .find(|(c, _)| *c == column)
      .map_or(SqlValue::Null, |(_, v)| v.clone())
  };
  let (yes, no) = (ctx.labels.ui("admin.yes"), ctx.labels.ui("admin.no"));
  let label = ctx.entity_label(&spec);
  let id = id_text(&value(spec.id_field().column));
  let list = site.list_path(&spec.slug);
  let title = format!("{label} {id}");
  let content = html! {
    section.admin-detail {
      (ctx.crumbs(&site, &[(label.clone(), Some(list.clone())), (id.clone(), None)]))
      h1 { (title) }
      dl.admin-fields {
        @for at in &spec.detail {
          dt { (ctx.field_label(&spec, *at)) }
          dd { (display(&value(spec.fields[*at].column), &yes, &no)) }
        }
      }
      p { a.admin-back href=(list) { (ctx.labels.ui("admin.back")) } }
    }
  };
  ctx.page(&site, &title, content, StatusCode::OK)
}
