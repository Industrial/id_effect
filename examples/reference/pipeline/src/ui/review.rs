//! The review queue (D1 stage 5): the tenant's documents waiting for review, one panel per
//! document, and a form that accepts it (with a corrected title) and publishes it.
//!
//! | Route | What | Guard |
//! |---|---|---|
//! | `GET /review` | the queue; with `HX-Target: review-queue` only its table | `reviewer` of the tenant |
//! | `GET /review/{id}` | a document's panel (the page, or the panel alone for htmx) | `reviewer` of the tenant |
//! | `POST /review/{id}` | accept: correct the title, publish (the outbox event commits with it) | `reviewer` of the tenant |
//! | `POST /review/locale` | the user's language (`locale=en` or `de`) | `reviewer` or `admin` of the tenant |
//!
//! The accept form is decoded through a `Schema` ([`ReviewForm`]): a title that is empty or
//! longer than 200 characters is answered with the form again (`422`), the submitted value kept
//! and the error bound to the field, in the request's language. A document someone else changed
//! since the panel was rendered (its version) is `409`; one that is not in review any more is
//! `409` too, with its own message. Every read and write runs in the request's tenant, through
//! the tenant router: another tenant's document id is "not found".

use std::sync::Arc;

use axum::Form;
use axum::extract::Path;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use id_effect::schema::{self, HasSchema, ParseError, Schema};
use id_effect::{DynEnv, Effect, HNil, fail, hlist, succeed};
use id_effect_kit::authz::CurrentPrincipal;
use id_effect_kit::axum::route::{Endpoint, Guarded, RouteInfo, Router};
use id_effect_kit::axum::run_with_env;
use id_effect_kit::i18n::{Args, Locale, Localizer, t, translate_parse_error};
use id_effect_kit::sql::query_builder::sea_query::{Expr, ExprTrait};
use id_effect_kit::sql::{Page, SqlClientEnv, SqlClientService};
use id_effect_kit::tenancy::Tenancy;
use id_effect_kit::ui::form::{FormState, HtmlForm};
use id_effect_kit::ui::{Component, HxRedirect, HxTrigger, LayoutContext, Markup, Slots, html};

use super::LocalePreferences;
use super::view::{
  DETAIL_ID, LOCALE_PATH, Panel, QUEUE_CHANGED, QUEUE_ID, QUEUE_PATH, StatusBadge, respond,
};
use crate::App;
use crate::admin::ROLE_ADMIN;
use crate::domain::{self, DIALECT, Document, DocumentDetail, DocumentStatus, PipelineError};
use crate::stages::publish_hooks;
use crate::tenancy::{ROLE_REVIEWER, guard, publish_guard};

/// The most documents the queue shows (the oldest first).
pub const MAX_QUEUE: u64 = 50;

/// The longest title accepted, in characters.
pub const MAX_TITLE_CHARS: usize = 200;

/// The accept form: the (corrected) title, 1 to [`MAX_TITLE_CHARS`] characters, and the
/// document version the panel was rendered with (form fields are text).
#[derive(Debug)]
pub struct ReviewForm;

impl HasSchema for ReviewForm {
  type A = (String, String);
  type I = (String, String);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "title",
      schema::max_length(schema::non_empty(schema::string()), MAX_TITLE_CHARS),
      "version",
      schema::non_empty(schema::string()),
    )
  }
}

/// Why accepting a document did not publish it.
#[derive(Clone, Debug, PartialEq)]
pub enum ReviewFailure {
  /// No such document in the tenant.
  NotFound,
  /// It is not waiting for review (any more).
  NotInReview,
  /// Someone changed it since the panel was rendered.
  Stale,
  /// The database or a hook failed.
  Failed(PipelineError),
}

impl From<PipelineError> for ReviewFailure {
  fn from(e: PipelineError) -> Self {
    match e {
      PipelineError::NotFound(_) => Self::NotFound,
      PipelineError::Conflict(_) => Self::Stale,
      other => Self::Failed(other),
    }
  }
}

/// Runs `effect` to its value or its typed failure (`None` for a defect).
async fn run<A, E>(effect: Effect<A, E>) -> Result<A, Option<E>>
where
  A: Send + 'static,
  E: Send + 'static,
{
  run_with_env(HNil, move |_| effect)
    .await
    .into_result()
    .map_err(|cause| cause.fail_option())
}

/// The tenant's documents in review, oldest first (at most [`MAX_QUEUE`]), with their source,
/// labels and latest extraction (one query per relation).
pub fn queue(app: &Arc<App>, tenancy: &Tenancy) -> Effect<Vec<DocumentDetail>, PipelineError> {
  let (db, scoped) = (Arc::clone(&app.db), tenancy.clone());
  let read = domain::repo::<Document>(&app.db, tenancy)
    .find_page(
      Expr::col("status").eq(DocumentStatus::Review.as_str()),
      Page::new(MAX_QUEUE, 0),
    )
    .map_err(PipelineError::from)
    .flat_map(move |documents| {
      if documents.is_empty() {
        return succeed(Vec::new());
      }
      let ids = documents.iter().map(|d| d.id).collect();
      domain::documents_with_relations(&db, &scoped, ids)
    })
    .map(|loaded| {
      let mut details: Vec<DocumentDetail> = loaded.iter().map(domain::detail_of).collect();
      details.sort_by_key(|d| d.id);
      details
    });
  tenancy.scope(read)
}

/// Accepts the tenant's document `id` at `version`: sets its title to `title` and publishes it,
/// with the publication's outbox event in the same transaction (the document's
/// `after_update` hook, as `stages::publish`).
pub fn accept(
  app: &Arc<App>,
  tenancy: &Tenancy,
  id: i64,
  version: i64,
  title: String,
) -> Effect<Document, ReviewFailure> {
  let (db, tenant) = (Arc::clone(&app.db), tenancy.tenant().as_str().to_owned());
  let hooked = tenancy
    .repository::<Document>(DIALECT)
    .with_hooks(publish_hooks(&tenant));
  let write = domain::find_document(&db, tenancy, id)
    .map_err(ReviewFailure::from)
    .flat_map(move |mut document| {
      if document.status != DocumentStatus::Review.as_str() {
        return fail(ReviewFailure::NotInReview);
      }
      if document.version != version {
        return fail(ReviewFailure::Stale);
      }
      document.title = title;
      DocumentStatus::Published
        .as_str()
        .clone_into(&mut document.status);
      let env: SqlClientEnv = hlist![SqlClientService(db)];
      hooked
        .update::<SqlClientEnv, _>(document)
        .provide_all(env)
        .map_err(ReviewFailure::from)
    });
  let hub = app.status.clone();
  tenancy.scope(write).map(move |document| {
    hub.announce(&tenant, document.id, DocumentStatus::Published);
    document
  })
}

/// `state` with every field error in `loc`'s language (the `Schema` messages of
/// id_effect_i18n; an error without a translation keeps its English text).
pub fn translated(loc: &Localizer, state: FormState) -> FormState {
  state.translate(|error| {
    let issue = ParseError::new("", error.message());
    let text = translate_parse_error(loc.catalog(), loc.locale(), &issue).message;
    (text != error.message()).then_some(text)
  })
}

/// The queue's table region (what htmx reloads after a publication).
fn queue_table(loc: &Localizer, documents: &[DocumentDetail]) -> Markup {
  let count = Args::new().with("count", documents.len() as i64);
  let detail_target = format!("#{DETAIL_ID}");
  let reload = format!("{QUEUE_CHANGED} from:body");
  html! {
    div id=(QUEUE_ID) hx-get=(QUEUE_PATH) hx-trigger=(reload) hx-target="this" hx-swap="outerHTML" {
      @if documents.is_empty() {
        p.empty { (t(loc, "review-empty", Args::new())) }
      } @else {
        p { (t(loc, "review-count", count)) }
        table.queue {
          thead {
            tr {
              th scope="col" { (t(loc, "review-column-title", Args::new())) }
              th scope="col" { (t(loc, "review-column-source", Args::new())) }
              th scope="col" { (t(loc, "review-column-labels", Args::new())) }
              th scope="col" { (t(loc, "review-column-duplicate", Args::new())) }
            }
          }
          tbody {
            @for d in documents {
              @let href = format!("{QUEUE_PATH}/{}", d.id);
              tr data-document=(d.id) {
                td {
                  a href=(href) hx-get=(href) hx-target=(detail_target) hx-swap="outerHTML" hx-push-url="true" {
                    (d.title)
                  }
                }
                td { (d.source) }
                td { (d.labels.join(", ")) }
                td { @if let Some(other) = d.duplicate_of { (other) } }
              }
            }
          }
        }
      }
    }
  }
}

/// A document's panel: status, source, labels, extraction, text and (in review) the accept form
/// with `state`'s values and errors.
fn detail_panel(
  app: &App,
  loc: &Localizer,
  ctx: &LayoutContext,
  d: &DocumentDetail,
  state: &FormState,
) -> Markup {
  let action = format!("{QUEUE_PATH}/{}", d.id);
  let target = format!("#{DETAIL_ID}");
  let title_label = loc.format("review-field-title", &Args::new());
  let in_review = d.status == DocumentStatus::Review.as_str();
  let extraction = d.extraction.as_ref().map(|e| {
    crate::ai::extraction_labels(&app.messages, loc.locale())
      .into_iter()
      .map(|(field, label)| {
        let value = match field {
          "title" => e.title.clone(),
          "summary" => e.summary.clone(),
          "language" => e.language.clone(),
          _ => e.organization.clone().unwrap_or_default(),
        };
        (label, value)
      })
      .collect::<Vec<_>>()
  });
  let body = html! {
    p.meta {
      (StatusBadge::new(loc, &d.status).alone()) " · " (d.source)
    }
    @if let Some(other) = d.duplicate_of {
      p.warning role="note" { (t(loc, "review-duplicate", Args::new().with("id", other))) }
    }
    h3 { (t(loc, "review-column-labels", Args::new())) }
    @if d.labels.is_empty() {
      p.empty { (t(loc, "review-no-labels", Args::new())) }
    } @else {
      ul.labels { @for label in &d.labels { li { (label) } } }
    }
    h3 { (t(loc, "review-extraction", Args::new())) }
    @match &extraction {
      Some(fields) => {
        dl.extraction { @for (label, value) in fields { dt { (label) } dd { (value) } } }
      }
      None => { p.empty { (t(loc, "review-no-extraction", Args::new())) } }
    }
    h3 { (t(loc, "review-body", Args::new())) }
    pre.body { (d.body) }
  };
  let form = html! {
    @if in_review {
      form.review method="post" action=(action) hx-post=(action) hx-target=(target) hx-swap="outerHTML" {
        (ctx.csrf_field())
        (state.error_summary())
        (state.field("title", &title_label).required())
        input type="hidden" name="version" value=(state.value("version"));
        button type="submit" { (t(loc, "review-accept", Args::new())) }
      }
    } @else {
      p.notice role="status" { (t(loc, "review-not-in-review", Args::new())) }
    }
  };
  Panel {
    id: DETAIL_ID,
    title: d.title.clone(),
  }
  .with(Slots::new().child(body).slot("footer", form))
  .render_markup()
}

/// Renders a slotted component as markup (a shorthand for `Render::render`).
trait RenderMarkup {
  fn render_markup(&self) -> Markup;
}

impl<C: Component> RenderMarkup for id_effect_kit::ui::Slotted<C> {
  fn render_markup(&self) -> Markup {
    id_effect_kit::ui::Render::render(self)
  }
}

/// A panel holding only `message` (a document that cannot be shown), with `status`.
fn message_panel(loc: &Localizer, key: &str) -> Markup {
  Panel {
    id: DETAIL_ID,
    title: loc.format("review-title", &Args::new()),
  }
  .with(Slots::new().child(html! { p.notice role="alert" { (t(loc, key, Args::new())) } }))
  .render_markup()
}

/// The whole queue page: the table and an empty document panel.
fn queue_content(loc: &Localizer, documents: &[DocumentDetail]) -> Markup {
  let title = loc.format("review-title", &Args::new());
  html! {
    h1 { (title) }
    (queue_table(loc, documents))
    (Panel { id: DETAIL_ID, title: loc.format("review-open", &Args::new()) }
      .with(Slots::new().child(html! { p.empty { (t(loc, "review-select", Args::new())) } })))
  }
}

/// `GET /review`.
async fn queue_page(
  app: Arc<App>,
  tenancy: Tenancy,
  loc: Localizer,
  ctx: LayoutContext,
) -> Response {
  let tenant = tenancy.tenant().as_str().to_owned();
  let title = loc.format("review-title", &Args::new());
  match run(queue(&app, &tenancy)).await {
    Ok(documents) => {
      let content = if ctx.hx().target.as_deref() == Some(QUEUE_ID) {
        queue_table(&loc, &documents)
      } else {
        queue_content(&loc, &documents)
      };
      respond(&loc, &ctx, &tenant, &title, content, StatusCode::OK)
    }
    Err(_) => error_response(&loc, &ctx, &tenant, StatusCode::INTERNAL_SERVER_ERROR),
  }
}

fn error_response(
  loc: &Localizer,
  ctx: &LayoutContext,
  tenant: &str,
  status: StatusCode,
) -> Response {
  let key = match status {
    StatusCode::NOT_FOUND => "review-not-found",
    _ => "review-error",
  };
  let title = loc.format("review-title", &Args::new());
  respond(loc, ctx, tenant, &title, message_panel(loc, key), status)
}

/// `GET /review/{id}`.
async fn document_page(
  app: Arc<App>,
  tenancy: Tenancy,
  loc: Localizer,
  ctx: LayoutContext,
  id: i64,
) -> Response {
  let tenant = tenancy.tenant().as_str().to_owned();
  let read = tenancy.scope(domain::document_detail(&app.db, &tenancy, id));
  match run(read).await {
    Ok(d) => {
      let state = FormState::new()
        .with_value("title", d.title.clone())
        .with_value("version", d.version.to_string());
      let panel = detail_panel(&app, &loc, &ctx, &d, &state);
      respond(&loc, &ctx, &tenant, &d.title, panel, StatusCode::OK)
    }
    Err(Some(PipelineError::NotFound(_))) => {
      error_response(&loc, &ctx, &tenant, StatusCode::NOT_FOUND)
    }
    Err(_) => error_response(&loc, &ctx, &tenant, StatusCode::INTERNAL_SERVER_ERROR),
  }
}

/// `POST /review/{id}`.
async fn review_document(
  app: Arc<App>,
  tenancy: Tenancy,
  loc: Localizer,
  ctx: LayoutContext,
  id: i64,
  form: HtmlForm<ReviewForm>,
) -> Response {
  let tenant = tenancy.tenant().as_str().to_owned();
  let read = tenancy.scope(domain::document_detail(&app.db, &tenancy, id));
  let current = match run(read).await {
    Ok(d) => d,
    Err(Some(PipelineError::NotFound(_))) => {
      return error_response(&loc, &ctx, &tenant, StatusCode::NOT_FOUND);
    }
    Err(_) => return error_response(&loc, &ctx, &tenant, StatusCode::INTERNAL_SERVER_ERROR),
  };
  // The form again, with what was submitted and what is wrong with it.
  let again = |state: FormState, status: StatusCode| {
    let panel = detail_panel(&app, &loc, &ctx, &current, &translated(&loc, state));
    respond(&loc, &ctx, &tenant, &current.title, panel, status)
  };
  let ((title, version), state) = match form.into_result() {
    Ok(decoded) => decoded,
    Err(state) => {
      let state = state.with_error("", loc.format("review-invalid", &Args::new()));
      return again(state, StatusCode::UNPROCESSABLE_ENTITY);
    }
  };
  let title = title.trim().to_owned();
  if title.is_empty() {
    let state = state.with_error("title", "must not be empty");
    return again(state, StatusCode::UNPROCESSABLE_ENTITY);
  }
  let Ok(version) = version.trim().parse::<i64>() else {
    return error_response(&loc, &ctx, &tenant, StatusCode::BAD_REQUEST);
  };
  match run(accept(&app, &tenancy, id, version, title)).await {
    Ok(document) => {
      if !ctx.hx().is_htmx() {
        return Redirect::to(QUEUE_PATH).into_response();
      }
      let notice = Panel {
        id: DETAIL_ID,
        title: document.title.clone(),
      }
      .with(Slots::new().child(html! {
        p.notice role="status" {
          (t(&loc, "review-published", Args::new().with("title", document.title.clone())))
        }
      }))
      .render_markup();
      (HxTrigger::event(QUEUE_CHANGED), notice).into_response()
    }
    Err(Some(ReviewFailure::NotFound)) => {
      error_response(&loc, &ctx, &tenant, StatusCode::NOT_FOUND)
    }
    Err(Some(ReviewFailure::NotInReview)) => {
      let message = loc.format("review-not-in-review", &Args::new());
      again(state.with_error("", message), StatusCode::CONFLICT)
    }
    Err(Some(ReviewFailure::Stale)) => {
      let message = loc.format("review-conflict", &Args::new());
      again(state.with_error("", message), StatusCode::CONFLICT)
    }
    Err(Some(ReviewFailure::Failed(_)) | None) => {
      error_response(&loc, &ctx, &tenant, StatusCode::INTERNAL_SERVER_ERROR)
    }
  }
}

/// `POST /review/locale`: the principal's language, when the catalog speaks it.
fn choose_locale(
  preferences: &LocalePreferences,
  loc: &Localizer,
  ctx: &LayoutContext,
  principal: &CurrentPrincipal,
  pairs: &[(String, String)],
) -> Response {
  let chosen = pairs
    .iter()
    .rev()
    .find(|(k, _)| k == "locale")
    .and_then(|(_, v)| Locale::parse(v).ok())
    .filter(|l| loc.catalog().supports(l));
  let Some(locale) = chosen else {
    let tenant = principal.tenant().unwrap_or_default().to_owned();
    let title = loc.format("review-title", &Args::new());
    let panel = message_panel(loc, "review-bad-locale");
    return respond(loc, ctx, &tenant, &title, panel, StatusCode::BAD_REQUEST);
  };
  preferences.set(principal, locale);
  if ctx.hx().is_htmx() {
    return match HxRedirect::to(QUEUE_PATH) {
      Ok(redirect) => (redirect, StatusCode::NO_CONTENT).into_response(),
      Err(e) => e.to_string().into_response(),
    };
  }
  Redirect::to(QUEUE_PATH).into_response()
}

/// The guard of the queue: a `reviewer` with a tenant claim.
pub fn review_guard() -> id_effect_kit::authz::Guard {
  publish_guard()
}

/// The guard of the language switch: a `reviewer` or an `admin` with a tenant claim.
pub fn locale_guard() -> id_effect_kit::authz::Guard {
  guard("pipeline.backoffice", &[ROLE_REVIEWER, ROLE_ADMIN])
}

/// The queue's typed routes over `app` (the language switch records into `preferences`).
pub fn routes(app: &Arc<App>, preferences: &LocalePreferences) -> Router<DynEnv> {
  Router::new()
    .route(queue_endpoint(app))
    .route(document_endpoint(app))
    .route(review_endpoint(app))
    .route(locale_endpoint(preferences))
}

fn queue_endpoint(app: &Arc<App>) -> Endpoint<DynEnv, Guarded> {
  let app = Arc::clone(app);
  let handler = move |tenancy: Tenancy, loc: Localizer, ctx: LayoutContext| {
    let app = Arc::clone(&app);
    async move { queue_page(app, tenancy, loc, ctx).await }
  };
  Endpoint::from_parts(
    RouteInfo::new(Method::GET, QUEUE_PATH),
    axum::routing::get(handler),
  )
  .guard(review_guard())
}

fn document_endpoint(app: &Arc<App>) -> Endpoint<DynEnv, Guarded> {
  let app = Arc::clone(app);
  let handler = move |tenancy: Tenancy, loc: Localizer, ctx: LayoutContext, Path(id): Path<i64>| {
    let app = Arc::clone(&app);
    async move { document_page(app, tenancy, loc, ctx, id).await }
  };
  Endpoint::from_parts(
    RouteInfo::new(Method::GET, format!("{QUEUE_PATH}/{{id}}")),
    axum::routing::get(handler),
  )
  .guard(review_guard())
}

fn review_endpoint(app: &Arc<App>) -> Endpoint<DynEnv, Guarded> {
  let app = Arc::clone(app);
  let handler = move |tenancy: Tenancy,
                      loc: Localizer,
                      ctx: LayoutContext,
                      Path(id): Path<i64>,
                      form: HtmlForm<ReviewForm>| {
    let app = Arc::clone(&app);
    async move { review_document(app, tenancy, loc, ctx, id, form).await }
  };
  Endpoint::from_parts(
    RouteInfo::new(Method::POST, format!("{QUEUE_PATH}/{{id}}")),
    axum::routing::post(handler),
  )
  .guard(review_guard())
}

fn locale_endpoint(preferences: &LocalePreferences) -> Endpoint<DynEnv, Guarded> {
  let preferences = preferences.clone();
  let handler = move |loc: Localizer,
                      ctx: LayoutContext,
                      axum::Extension(principal): axum::Extension<CurrentPrincipal>,
                      Form(pairs): Form<Vec<(String, String)>>| {
    let preferences = preferences.clone();
    async move { choose_locale(&preferences, &loc, &ctx, &principal, &pairs) }
  };
  Endpoint::from_parts(
    RouteInfo::new(Method::POST, LOCALE_PATH),
    axum::routing::post(handler),
  )
  .guard(locale_guard())
}

#[cfg(test)]
mod tests {
  use id_effect_kit::authz::BasicPrincipal;

  use super::*;

  fn localizer(tag: &str) -> Localizer {
    Localizer::new(super::super::catalog(), Locale::parse(tag).unwrap())
  }

  #[test]
  fn the_form_schema_bounds_the_title() {
    let schema = ReviewForm::schema();
    assert!(schema.decode(("Q3".to_owned(), "1".to_owned())).is_ok());
    let long = "x".repeat(MAX_TITLE_CHARS + 1);
    assert!(schema.decode((long, "1".to_owned())).is_err());
    assert!(schema.decode((String::new(), "1".to_owned())).is_err());
  }

  #[test]
  fn field_errors_are_translated_and_unknown_ones_kept() {
    let state = FormState::new()
      .with_error("title", "must not be empty")
      .with_error("title", "a message nobody translates");
    let de = translated(&localizer("de"), state);
    let messages: Vec<&str> = de.errors("title").iter().map(|e| e.message()).collect();
    assert_eq!(messages[0], "Darf nicht leer sein.");
    assert_eq!(messages[1], "a message nobody translates");
  }

  #[test]
  fn failures_map_from_pipeline_errors() {
    assert_eq!(
      ReviewFailure::from(PipelineError::NotFound("x".into())),
      ReviewFailure::NotFound
    );
    assert_eq!(
      ReviewFailure::from(PipelineError::Conflict("x".into())),
      ReviewFailure::Stale
    );
    assert!(matches!(
      ReviewFailure::from(PipelineError::NoTenant),
      ReviewFailure::Failed(_)
    ));
  }

  #[test]
  fn the_language_switch_accepts_only_supported_locales() {
    let prefs = LocalePreferences::new();
    let en = localizer("en");
    let ctx = LayoutContext::default();
    let rita = CurrentPrincipal::new(BasicPrincipal::new("rita").with_tenant("acme"));
    let pairs = |v: &str| vec![("locale".to_owned(), v.to_owned())];
    let bad = choose_locale(&prefs, &en, &ctx, &rita, &pairs("fr"));
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert!(prefs.is_empty());
    let ok = choose_locale(&prefs, &en, &ctx, &rita, &pairs("de"));
    assert_eq!(ok.status(), StatusCode::SEE_OTHER);
    assert_eq!(
      prefs.get(&rita).map(|l| l.as_str().to_owned()),
      Some("de".to_owned())
    );
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("hx-request", "true".parse().unwrap());
    let htmx =
      LayoutContext::default().with_hx(id_effect_kit::ui::HxRequest::from_headers(&headers));
    let hx = choose_locale(&prefs, &en, &htmx, &rita, &pairs("en"));
    assert_eq!(hx.status(), StatusCode::NO_CONTENT);
    assert_eq!(hx.headers().get("hx-redirect").unwrap(), QUEUE_PATH);
  }
}
