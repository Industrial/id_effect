//! The list page (web.admin #1 read half, #2): the entity's rows with text search over the
//! declared columns, typed filters, sortable columns and keyset pages, rendered with maud and
//! updated in place by htmx.
//!
//! The page reads its state from the query string, so every view is a link:
//!
//! | Parameter | Meaning |
//! |---|---|
//! | `q` | search text: rows where any search field contains it (case-insensitive) |
//! | `f.<field>` | a filter value (a declared choice, or a value of the field's type) |
//! | `sort`, `dir` | a sortable field and `asc` / `desc` |
//! | `after`, `before` | the keyset cursor of the next / previous page |
//!
//! Untrusted input is bounded (ADR 0013 R4): a query string over [`MAX_QUERY_LEN`] bytes answers
//! 414, search text is cut to [`MAX_SEARCH_CHARS`] characters, a filter value that is not a
//! declared choice or does not parse is ignored, and a sort field that is not declared sortable
//! is ignored. A cursor that does not decode, or was issued for another search, filter or sort,
//! answers 400 (the cursor's fingerprint, ADR 0015 D11). The search is a parameterized `LIKE`
//! with `%`, `_` and `\` escaped.
//!
//! An htmx request whose target is the results region (`HX-Target: admin-results`: the search
//! form, the sort headers, the pager) gets only that region; any other request gets the page.

use std::sync::Arc;

use axum::extract::{Query, Request};
use axum::http::{StatusCode, Uri};
use axum::response::Response;
use id_effect::Cause;
use id_effect::Exit;
use id_effect_axum::EffectExecutor;
use id_effect_axum::ui::{Markup, html};
use id_effect_sql::query_builder::sea_query::{Condition, Expr, ExprTrait, Func, LikeExpr, Value};
use id_effect_sql::repository::{Keyset, PageCursor, Paged, SortOrder};
use id_effect_sql::{Entity, SqlError, SqlValue};

use crate::entity::EntitySpec;
use crate::permission::AdminAction;
use crate::site::{Ctx, SiteShared};
use crate::value::{ValueKind, display, id_text, query_string, truncate};

/// The longest query string a list page accepts, in bytes.
pub const MAX_QUERY_LEN: usize = 2048;
/// The longest search text used, in characters (longer text is cut).
pub const MAX_SEARCH_CHARS: usize = 100;
/// The id of the results region htmx swaps.
pub const RESULTS_ID: &str = "admin-results";
/// The longest cell text on a list page, in characters.
const CELL_CHARS: usize = 80;

/// Which way a cursor pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dir {
  After,
  Before,
}

/// A list request, parsed and checked against the entity.
#[derive(Clone, Debug, Default)]
pub(crate) struct ListQuery {
  q: Option<String>,
  /// The requested sort: a sortable field and the order.
  sort: Option<(usize, SortOrder)>,
  /// The applied filters: index into the spec's filters, the request text and the value.
  filters: Vec<(usize, String, SqlValue)>,
  cursor: Option<(Dir, PageCursor)>,
}

impl ListQuery {
  /// The list request in `uri` for `spec`, or the status to answer (414 too long, 400 bad
  /// encoding or cursor).
  pub(crate) fn parse(uri: &Uri, spec: &EntitySpec) -> Result<Self, StatusCode> {
    if uri.query().is_some_and(|q| q.len() > MAX_QUERY_LEN) {
      return Err(StatusCode::URI_TOO_LONG);
    }
    let Query(pairs) =
      Query::<Vec<(String, String)>>::try_from_uri(uri).map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut query = Self::default();
    let (mut sort, mut desc, mut after, mut before) = (None, false, None, None);
    let mut raw_filters: Vec<(String, String)> = Vec::new();
    for (key, value) in pairs {
      match key.as_str() {
        "q" => query.q = Some(value),
        "sort" => sort = Some(value),
        "dir" => desc = value == "desc",
        "after" => after = Some(value),
        "before" => before = Some(value),
        other => {
          if let Some(field) = other.strip_prefix("f.") {
            raw_filters.push((field.to_owned(), value));
          }
        }
      }
    }
    query.q = query
      .q
      .map(|q| q.trim().chars().take(MAX_SEARCH_CHARS).collect::<String>())
      .filter(|q| !q.is_empty() && !spec.search.is_empty());
    let order = if desc {
      SortOrder::Desc
    } else {
      SortOrder::Asc
    };
    query.sort = sort.and_then(|name| {
      spec
        .sortable
        .iter()
        .find(|at| spec.fields[**at].name == name)
        .map(|at| (*at, order))
    });
    for (at, filter) in spec.filters.iter().enumerate() {
      let name = spec.fields[filter.field].name;
      let Some((_, text)) = raw_filters
        .iter()
        .rev()
        .find(|(n, v)| n == name && !v.is_empty())
      else {
        continue;
      };
      let value = match &filter.choices {
        Some(choices) => choices
          .iter()
          .find(|(choice, _)| choice == text)
          .map(|(_, v)| v.clone()),
        None => filter.kind.parse(text),
      };
      if let Some(value) = value {
        query.filters.push((at, text.clone(), value));
      }
    }
    let decode = |token: String| PageCursor::decode(&token).map_err(|_| StatusCode::BAD_REQUEST);
    query.cursor = match (after, before) {
      (Some(_), Some(_)) => return Err(StatusCode::BAD_REQUEST),
      (Some(token), None) => Some((Dir::After, decode(token)?)),
      (None, Some(token)) => Some((Dir::Before, decode(token)?)),
      (None, None) => None,
    };
    Ok(query)
  }

  /// The rows the request selects: every filter, and the search over any search column.
  pub(crate) fn condition(&self, spec: &EntitySpec) -> Condition {
    let mut cond = Condition::all();
    for (at, _, value) in &self.filters {
      let column = spec.fields[spec.filters[*at].field].column;
      cond = cond.add(Expr::col(column).eq(Value::from(value.clone())));
    }
    if let Some(q) = &self.q {
      let pattern = format!("%{}%", escape_like(&q.to_lowercase()));
      let any = spec.search.iter().fold(Condition::any(), |any, column| {
        any.add(
          Expr::expr(Func::lower(Expr::col(*column)))
            .like(LikeExpr::new(pattern.clone()).escape('\\')),
        )
      });
      cond = cond.add(any);
    }
    cond
  }

  /// The order rows are listed in: the requested sort, else the entity's default.
  fn order(&self, spec: &EntitySpec) -> Option<(usize, SortOrder)> {
    self.sort.or(spec.default_sort)
  }

  /// The keyset of the request (its filter and order; the page size of the entity).
  fn keyset(&self, spec: &EntitySpec, cond: Condition) -> Keyset {
    let keyset = Keyset::new().size(spec.page_size).filter(cond);
    match self.order(spec) {
      Some((at, order)) => keyset.order_by(spec.fields[at].column, order),
      None => keyset,
    }
  }

  /// The query parameters that reproduce this view (search, filters, sort), without a cursor.
  fn params(&self, spec: &EntitySpec) -> Vec<(String, String)> {
    let mut params = Vec::new();
    if let Some(q) = &self.q {
      params.push(("q".to_owned(), q.clone()));
    }
    for (at, text, _) in &self.filters {
      let name = spec.fields[spec.filters[*at].field].name;
      params.push((format!("f.{name}"), text.clone()));
    }
    if let Some((at, order)) = self.sort {
      params.push(("sort".to_owned(), spec.fields[at].name.to_owned()));
      params.push(("dir".to_owned(), dir_name(order).to_owned()));
    }
    params
  }

  /// The text of the filter on `field` in this request, if one applies.
  fn filter_text(&self, filter: usize) -> Option<&str> {
    self
      .filters
      .iter()
      .find(|(at, _, _)| *at == filter)
      .map(|(_, text, _)| text.as_str())
  }
}

fn dir_name(order: SortOrder) -> &'static str {
  match order {
    SortOrder::Asc => "asc",
    SortOrder::Desc => "desc",
  }
}

/// `text` with the `LIKE` wildcards and the escape character escaped by `\`.
pub(crate) fn escape_like(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  for c in text.chars() {
    if matches!(c, '%' | '_' | '\\') {
      out.push('\\');
    }
    out.push(c);
  }
  out
}

/// `path` with `params` as its query string.
fn link(path: &str, params: &[(String, String)]) -> String {
  format!(
    "{path}{}",
    query_string(params.iter().map(|(k, v)| (k.as_str(), v.as_str())))
  )
}

/// `GET <prefix>/<entity>`.
pub(crate) async fn list<E: Entity>(
  site: Arc<SiteShared>,
  spec: Arc<EntitySpec>,
  req: Request,
) -> Response {
  let (parts, _body) = req.into_parts();
  let ctx = Ctx::new(&site, parts);
  if let Err(denied) = ctx.require(&site, &spec, AdminAction::List).await {
    return *denied;
  }
  let repo = match ctx.repository::<E>(&site) {
    Ok(repo) => repo,
    Err(refused) => return *refused,
  };
  let query = match ListQuery::parse(&ctx.parts.uri, &spec) {
    Ok(query) => query,
    Err(status) => return ctx.error(&site, status, "admin.bad-request"),
  };
  let can_view = ctx.allowed(&site, &spec, AdminAction::View).await.is_ok();
  let bulk = crate::bulk::options(&ctx, &site, &spec).await;
  let new = crate::edit::new_link(&ctx, &site, &spec).await;
  // The statements are rendered here; the (non-`Send`) conditions end with this block.
  let read = {
    let cond = query.condition(&spec);
    let keyset = query.keyset(&spec, cond.clone());
    let rows = repo.on(site.db.client());
    let count = rows.count_by(cond);
    let page = match query.cursor.clone() {
      Some((Dir::Before, cursor)) => rows.find_before(&keyset, Some(cursor)),
      Some((Dir::After, cursor)) => rows.find_after(&keyset, Some(cursor)),
      None => rows.find_after(&keyset, None),
    };
    page.flat_map(move |page| count.map(move |total| (page, total)))
  };
  match EffectExecutor::global().run_effect(read).await {
    Exit::Success((page, total)) => {
      let results = results(&ctx, &site, &spec, &query, &page, total, can_view, &bulk);
      let wants_results = ctx.layout.hx().target.as_deref() == Some(RESULTS_ID);
      let label = ctx.entity_label(&spec);
      let content = if wants_results && ctx.layout.hx().is_htmx() {
        results
      } else {
        list_page(&ctx, &site, &spec, &query, &label, new, results)
      };
      ctx.page(&site, &label, content, StatusCode::OK)
    }
    // A cursor this request cannot use (issued for another search, filter or sort).
    Exit::Failure(Cause::Fail(SqlError::Decode(_))) if query.cursor.is_some() => {
      ctx.error(&site, StatusCode::BAD_REQUEST, "admin.bad-request")
    }
    Exit::Failure(cause) => {
      tracing::error!(target: "id_effect_admin", entity = spec.entity, ?cause, "admin list failed");
      ctx.error(&site, StatusCode::INTERNAL_SERVER_ERROR, "admin.error")
    }
  }
}

/// The whole list page: heading, search and filter form, results region.
fn list_page(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  query: &ListQuery,
  label: &str,
  new: Markup,
  results: Markup,
) -> Markup {
  let path = site.list_path(&spec.slug);
  let target = format!("#{RESULTS_ID}");
  let has_form = !spec.search.is_empty() || !spec.filters.is_empty();
  let any = ctx.labels.ui("admin.any");
  html! {
    section.admin-list {
      (ctx.crumbs(site, &[(label.to_owned(), None)]))
      h1 { (label) }
      (new)
      @if has_form {
        form.admin-search role="search" method="get" action=(path)
          hx-get=(path) hx-target=(target) hx-push-url="true" hx-trigger="submit, change" {
          @if !spec.search.is_empty() {
            input type="search" name="q" value=[query.q.as_deref()]
              placeholder=(ctx.labels.ui("admin.search")) aria-label=(ctx.labels.ui("admin.search"));
          }
          @for (at, filter) in spec.filters.iter().enumerate() {
            @let field = &spec.fields[filter.field];
            @let name = format!("f.{}", field.name);
            @let current = query.filter_text(at);
            label.admin-filter {
              (ctx.field_label(spec, filter.field)) " "
              @if let Some(choices) = &filter.choices {
                select name=(name) {
                  option value="" { (any) }
                  @for (choice, _) in choices {
                    option value=(choice) selected[current == Some(choice.as_str())] { (choice) }
                  }
                }
              } @else if filter.kind == ValueKind::Bool {
                select name=(name) {
                  option value="" { (any) }
                  option value="true" selected[current == Some("true")] { (ctx.labels.ui("admin.yes")) }
                  option value="false" selected[current == Some("false")] { (ctx.labels.ui("admin.no")) }
                }
              } @else {
                input type="text" name=(name) value=[current];
              }
            }
          }
          @if let Some((at, order)) = query.sort {
            input type="hidden" name="sort" value=(spec.fields[at].name);
            input type="hidden" name="dir" value=(dir_name(order));
          }
          button type="submit" { (ctx.labels.ui("admin.apply")) }
        }
      }
      (results)
    }
  }
}

/// The results region: count, table (in the bulk form when `bulk` offers actions), pager.
#[allow(clippy::too_many_arguments)]
fn results<E: Entity>(
  ctx: &Ctx,
  site: &SiteShared,
  spec: &EntitySpec,
  query: &ListQuery,
  page: &Paged<E>,
  total: u64,
  can_view: bool,
  bulk: &[(String, String)],
) -> Markup {
  let path = site.list_path(&spec.slug);
  let target = format!("#{RESULTS_ID}");
  let params = query.params(spec);
  let order = query.order(spec);
  let (yes, no) = (ctx.labels.ui("admin.yes"), ctx.labels.ui("admin.no"));
  let id_column = spec.id_field().column;
  let rows: Vec<(String, Vec<String>)> = page
    .items
    .iter()
    .map(|row| {
      let values = row.values();
      let value = |column: &str| {
        values
          .iter()
          .find(|(c, _)| *c == column)
          .map_or(SqlValue::Null, |(_, v)| v.clone())
      };
      let cells = spec
        .list
        .iter()
        .map(|at| {
          truncate(
            display(&value(spec.fields[*at].column), &yes, &no),
            CELL_CHARS,
          )
        })
        .collect();
      (id_text(&value(id_column)), cells)
    })
    .collect();
  let sort_link = |at: usize| {
    let next = match order {
      Some((current, SortOrder::Asc)) if current == at => SortOrder::Desc,
      _ => SortOrder::Asc,
    };
    let mut params: Vec<(String, String)> = params
      .iter()
      .filter(|(k, _)| k != "sort" && k != "dir")
      .cloned()
      .collect();
    params.push(("sort".to_owned(), spec.fields[at].name.to_owned()));
    params.push(("dir".to_owned(), dir_name(next).to_owned()));
    link(&path, &params)
  };
  let page_link = |key: &str, cursor: &PageCursor| {
    let mut params = params.clone();
    params.push((key.to_owned(), cursor.encode()));
    link(&path, &params)
  };
  let select = !bulk.is_empty();
  let table = || {
    html! {
      table.admin-table {
        thead {
          tr {
            @if select { th scope="col" {} }
            @for at in &spec.list {
              @let sorted = order.filter(|(o, _)| o == at).map(|(_, dir)| match dir {
                SortOrder::Asc => "ascending",
                SortOrder::Desc => "descending",
              });
              th scope="col" aria-sort=[sorted] {
                @if spec.sortable.contains(at) {
                  @let href = sort_link(*at);
                  a href=(href) hx-get=(href) hx-target=(target) hx-push-url="true" {
                    (ctx.field_label(spec, *at))
                  }
                } @else {
                  (ctx.field_label(spec, *at))
                }
              }
            }
          }
        }
        tbody {
          @for (id, cells) in &rows {
            tr {
              @if select { td { (crate::bulk::checkbox(ctx, id)) } }
              @for (i, cell) in cells.iter().enumerate() {
                td {
                  @if i == 0 && can_view {
                    a href=(site.detail_path(&spec.slug, id)) { (cell) }
                  } @else {
                    (cell)
                  }
                }
              }
            }
          }
        }
      }
    }
  };
  html! {
    div #(RESULTS_ID) .admin-results {
      p.admin-count { (ctx.labels.results(total)) }
      @if rows.is_empty() {
        p.admin-empty { (ctx.labels.ui("admin.empty")) }
      } @else if select {
        (crate::bulk::form(ctx, site, spec, bulk, table()))
      } @else {
        (table())
      }
      @if page.prev.is_some() || page.next.is_some() {
        nav.admin-pager aria-label="pagination" {
          @if let Some(prev) = &page.prev {
            @let href = page_link("before", prev);
            a rel="prev" href=(href) hx-get=(href) hx-target=(target) hx-push-url="true" {
              (ctx.labels.ui("admin.previous"))
            }
          }
          @if let Some(next) = &page.next {
            @let href = page_link("after", next);
            a rel="next" href=(href) hx-get=(href) hx-target=(target) hx-push-url="true" {
              (ctx.labels.ui("admin.next"))
            }
          }
        }
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn like_wildcards_are_escaped() {
    assert_eq!(escape_like(r"50%_off\"), r"50\%\_off\\");
    assert_eq!(escape_like("plain"), "plain");
  }

  #[test]
  fn links_encode_their_parameters() {
    let params = vec![("q".to_owned(), "a b".to_owned())];
    assert_eq!(link("/admin/doc", &params), "/admin/doc?q=a%20b");
    assert_eq!(link("/admin/doc", &[]), "/admin/doc");
    assert_eq!(dir_name(SortOrder::Desc), "desc");
  }
}
