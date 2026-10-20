//! Route table introspection (feature **`openapi`**): what every typed route of a
//! [`RouteRegistry`] declares — method, path template, handler name, the schema of each request
//! part, documented responses and guarding policies — as JSON ([`route_table`]), as a text table
//! ([`route_table_text`]), or served at `GET /_routes` by [`routes_router`].
//!
//! Nothing is mounted by default: merge [`routes_router`] into your app where you want the listing
//! (a development build, or behind an admin guard — it reveals the API's internals). It answers
//! JSON, or the text table for `?format=text` and for clients that accept `text/plain` but not
//! JSON (`curl -H 'accept: text/plain'`).
//!
//! ```rust
//! use axum::http::StatusCode;
//! use id_effect::schema::{self, HasSchema, Schema};
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::route::introspect::{route_table, route_table_text, routes_router};
//! use id_effect_axum::route::{Input, Route, Router};
//!
//! struct AccountId;
//!
//! impl HasSchema for AccountId {
//!   type A = i64;
//!   type I = i64;
//!   type E = ();
//!
//!   fn schema() -> Schema<i64, i64> {
//!     schema::i64::<()>()
//!   }
//! }
//!
//! fn show(_: &mut (), input: Input<i64>) -> Effect<String, std::convert::Infallible, ()> {
//!   succeed(format!("account {}", input.path))
//! }
//!
//! let router = Router::new().route(
//!   Route::get("/accounts/{id}")
//!     .public()
//!     .path::<AccountId>()
//!     .response::<AccountId>(StatusCode::OK)
//!     .handler(show),
//! );
//! let table = route_table(router.registry());
//! assert_eq!(table[0].path, "/accounts/{id}");
//! assert!(table[0].handler.as_deref().unwrap().ends_with("::show"));
//! assert_eq!(table[0].path_params.as_deref(), Some("AccountId"));
//! assert_eq!(table[0].access, "public");
//! assert!(route_table_text(router.registry()).starts_with("METHOD"));
//!
//! let listing: axum::Router = routes_router(router.registry());
//! let app: axum::Router = router.into_axum().merge(listing);
//! # let _ = app;
//! ```

use axum::http::{HeaderMap, HeaderValue, Uri, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::{BodyKind, RouteInfo, RouteRegistry, SchemaRef};

/// Where [`routes_router`] serves the listing.
pub const ROUTES_PATH: &str = "/_routes";

/// One documented response of a [`RouteEntry`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ResponseEntry {
  /// Status code.
  pub status: u16,
  /// Body schema name ([`SchemaRef::short_name`]).
  pub schema: String,
}

/// One row of the route table: a [`RouteInfo`] with schemas by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct RouteEntry {
  /// HTTP method (`GET`).
  pub method: String,
  /// Path template (`/accounts/{id}`).
  pub path: String,
  /// Handler type name ([`RouteInfo::handler`]).
  pub handler: Option<String>,
  /// Path parameter schema name.
  pub path_params: Option<String>,
  /// Query schema name.
  pub query: Option<String>,
  /// Header schema name.
  pub headers: Option<String>,
  /// Body schema name.
  pub body: Option<String>,
  /// How the body is read (`json` or `form`), when a body is declared.
  pub body_kind: Option<String>,
  /// Documented responses, in declaration order.
  pub responses: Vec<ResponseEntry>,
  /// Guarding policy names ([`RouteInfo::policies`]).
  pub policies: Vec<String>,
  /// Access decision ([`RouteAccess::as_str`](super::RouteAccess::as_str): `public`,
  /// `guarded`, `unchecked`).
  pub access: String,
}

fn name(schema: Option<&SchemaRef>) -> Option<String> {
  schema.map(|s| s.short_name().to_owned())
}

impl From<&RouteInfo> for RouteEntry {
  fn from(info: &RouteInfo) -> Self {
    Self {
      method: info.method.as_str().to_owned(),
      path: info.path.clone(),
      handler: info.handler.map(str::to_owned),
      path_params: name(info.path_params.as_ref()),
      query: name(info.query.as_ref()),
      headers: name(info.headers.as_ref()),
      body: name(info.body.as_ref()),
      body_kind: info.body.as_ref().map(|_| {
        match info.body_kind {
          BodyKind::Json => "json",
          BodyKind::Form => "form",
        }
        .to_owned()
      }),
      responses: info
        .responses
        .iter()
        .map(|r| ResponseEntry {
          status: r.status.as_u16(),
          schema: r.schema.short_name().to_owned(),
        })
        .collect(),
      policies: info.policies.clone(),
      access: info.access.as_str().to_owned(),
    }
  }
}

/// One [`RouteEntry`] per route, in registration order (`route_table(router.registry())`).
pub fn route_table<'a>(routes: impl IntoIterator<Item = &'a RouteInfo>) -> Vec<RouteEntry> {
  routes.into_iter().map(RouteEntry::from).collect()
}

/// The route table as aligned text columns: `METHOD`, `PATH`, `HANDLER`, `INPUT` (`path=…`,
/// `query=…`, `headers=…`, `body=…` or `form=…`), `RESPONSES` and `POLICIES` (`-` when empty).
pub fn route_table_text<'a>(routes: impl IntoIterator<Item = &'a RouteInfo>) -> String {
  let header = [
    "METHOD",
    "PATH",
    "HANDLER",
    "INPUT",
    "RESPONSES",
    "POLICIES",
  ]
  .map(str::to_owned);
  let mut rows = vec![header];
  for entry in route_table(routes) {
    let body = if entry.body_kind.as_deref() == Some("form") {
      "form"
    } else {
      "body"
    };
    let input: Vec<String> = [
      ("path", &entry.path_params),
      ("query", &entry.query),
      ("headers", &entry.headers),
      (body, &entry.body),
    ]
    .into_iter()
    .filter_map(|(part, schema)| schema.as_ref().map(|s| format!("{part}={s}")))
    .collect();
    let responses: Vec<String> = entry
      .responses
      .iter()
      .map(|r| format!("{} {}", r.status, r.schema))
      .collect();
    let or_dash = |items: Vec<String>, sep: &str| {
      if items.is_empty() {
        "-".to_owned()
      } else {
        items.join(sep)
      }
    };
    rows.push([
      entry.method,
      entry.path,
      entry.handler.unwrap_or_else(|| "-".to_owned()),
      or_dash(input, " "),
      or_dash(responses, ", "),
      or_dash(entry.policies, ", "),
    ]);
  }
  let mut widths = [0usize; 6];
  for row in &rows {
    for (width, cell) in widths.iter_mut().zip(row) {
      *width = (*width).max(cell.chars().count());
    }
  }
  let mut out = String::new();
  for row in &rows {
    let mut line = String::new();
    for (cell, width) in row.iter().zip(widths) {
      line.push_str(cell);
      line.extend(std::iter::repeat_n(' ', width - cell.chars().count() + 2));
    }
    out.push_str(line.trim_end());
    out.push('\n');
  }
  out
}

/// Whether the request asks for the text table: `?format=text`, or an `Accept` that names
/// `text/plain` but not JSON (`?format=json` forces JSON).
fn wants_text(uri: &Uri, headers: &HeaderMap) -> bool {
  for pair in uri.query().unwrap_or_default().split('&') {
    match pair {
      "format=text" => return true,
      "format=json" => return false,
      _ => {}
    }
  }
  headers
    .get_all(header::ACCEPT)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .any(|accept| accept.contains("text/plain") && !accept.contains("json"))
}

/// An Axum router serving the route table of `registry` at [`ROUTES_PATH`] (`GET /_routes`):
/// JSON (an array of [`RouteEntry`]) by default, the [`route_table_text`] table on request.
/// Responses are `Cache-Control: no-store`. The table is a snapshot: routes registered later are
/// not listed. Never mounted unless you merge it.
pub fn routes_router<S>(registry: &RouteRegistry) -> axum::Router<S>
where
  S: Clone + Send + Sync + 'static,
{
  let json = axum::body::Bytes::from(
    serde_json::to_vec(&route_table(registry)).expect("a route table serializes to JSON"),
  );
  let text = axum::body::Bytes::from(route_table_text(registry));
  axum::Router::new().route(
    ROUTES_PATH,
    axum::routing::get(move |uri: Uri, headers: HeaderMap| {
      let (body, content_type) = if wants_text(&uri, &headers) {
        (text.clone(), "text/plain; charset=utf-8")
      } else {
        (json.clone(), "application/json")
      };
      std::future::ready(listing(body, content_type))
    }),
  )
}

fn listing(body: axum::body::Bytes, content_type: &'static str) -> Response {
  (
    [
      (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
      (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
    ],
    body,
  )
    .into_response()
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::http::{Method, StatusCode};
  use id_effect::schema::{self, HasSchema, Schema};

  struct Id;

  impl HasSchema for Id {
    type A = i64;
    type I = i64;
    type E = ();

    fn schema() -> Schema<i64, i64> {
      schema::i64::<()>()
    }
  }

  fn routes() -> Vec<RouteInfo> {
    let mut form = RouteInfo::new(Method::POST, "/ids");
    form.body = Some(SchemaRef::of::<Id>());
    form.body_kind = BodyKind::Form;
    form.policies = vec!["admin".to_owned(), "staff".to_owned()];
    let mut all = RouteInfo::new(Method::PUT, "/ids/{id}");
    all.handler = Some("app::put_id");
    all.path_params = Some(SchemaRef::of::<Id>());
    all.query = Some(SchemaRef::of::<Id>());
    all.headers = Some(SchemaRef::of::<Id>());
    all.body = Some(SchemaRef::of::<Id>());
    all.responses = vec![super::super::ResponseInfo {
      status: StatusCode::OK,
      schema: SchemaRef::of::<Id>(),
    }];
    vec![RouteInfo::new(Method::GET, "/"), form, all]
  }

  #[test]
  fn entries_name_every_part() {
    let routes = routes();
    // The handler name is descriptive: it does not take part in equality.
    let mut unnamed = routes[2].clone();
    unnamed.handler = None;
    assert_eq!(unnamed, routes[2]);
    assert_ne!(routes[1], routes[2]);
    let table = route_table(&routes);
    assert_eq!(table[0].handler, None);
    assert_eq!(table[2].handler.as_deref(), Some("app::put_id"));
    assert_eq!(table[0].body_kind, None);
    assert_eq!(table[1].body_kind.as_deref(), Some("form"));
    assert_eq!(table[2].body_kind.as_deref(), Some("json"));
    assert_eq!(table[2].headers.as_deref(), Some("Id"));
    assert_eq!(
      table[2].responses,
      [ResponseEntry {
        status: 200,
        schema: "Id".to_owned()
      }]
    );
  }

  #[test]
  fn text_table_aligns_columns_and_dashes_empty_cells() {
    let text = route_table_text(&routes());
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
      lines[0],
      "METHOD  PATH       HANDLER      INPUT                                RESPONSES  POLICIES"
    );
    assert_eq!(
      lines[1],
      "GET     /          -            -                                    -          -"
    );
    assert_eq!(
      lines[2],
      "POST    /ids       -            form=Id                              -          admin, staff"
    );
    assert_eq!(
      lines[3],
      "PUT     /ids/{id}  app::put_id  path=Id query=Id headers=Id body=Id  200 Id     -"
    );
  }

  #[test]
  fn format_is_negotiated() {
    let accept = |value: &'static str| {
      let mut headers = HeaderMap::new();
      headers.insert(header::ACCEPT, HeaderValue::from_static(value));
      headers
    };
    let plain: Uri = "/_routes".parse().unwrap();
    assert!(!wants_text(&plain, &HeaderMap::new()));
    assert!(wants_text(&plain, &accept("text/plain")));
    assert!(!wants_text(&plain, &accept("application/json, text/plain")));
    assert!(wants_text(
      &"/_routes?x=1&format=text".parse().unwrap(),
      &HeaderMap::new()
    ));
    assert!(!wants_text(
      &"/_routes?format=json".parse().unwrap(),
      &accept("text/plain")
    ));
  }
}
