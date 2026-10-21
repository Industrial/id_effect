//! htmx: request extractors, response headers and the vendored script served from the app.
//!
//! - **Request**: [`HxRequest`] reads every `HX-*` request header (is this an htmx request,
//!   boosted, a history restore, its target and trigger); [`HxTarget`] only the target.
//! - **Response**: [`HxTrigger`] (client events), [`HxRedirect`] (same origin only),
//!   [`HxReswap`] and [`HxRetarget`] are [`IntoResponseParts`], returned in a tuple before the
//!   body.
//! - **The script**: htmx 2.0.11 is compiled into the crate ([`HTMX_JS`], pinned by
//!   [`HTMX_INTEGRITY`]; see `assets/README.md`) and served by [`asset_route`] at [`HTMX_PATH`].
//!   [`htmx_head`] is the `<head>` partial loading it with its `integrity` and the hardened
//!   `htmx-config` ([`HTMX_CONFIG`]: no `eval`, no script tags in swapped content, same-origin
//!   requests only). There is no CDN: pages keep `script-src 'self'`.
//!
//! ```rust
//! use axum::http::StatusCode;
//! use id_effect_axum::ui::htmx::{HxRedirect, HxRequest, HxTrigger};
//! use id_effect_axum::ui::{Markup, html, maud};
//!
//! async fn save(hx: HxRequest) -> Result<(HxTrigger, Markup), (StatusCode, HxRedirect)> {
//!   if !hx.is_htmx() {
//!     let back = HxRedirect::to("/notes").expect("a same-origin path");
//!     return Err((StatusCode::SEE_OTHER, back));
//!   }
//!   Ok((HxTrigger::event("note-saved"), html! { p { "saved" } }))
//! }
//! # let _ = axum::routing::post(save);
//! assert!(HxRedirect::to("https://evil.example/").is_err());
//! ```

use std::convert::Infallible;
use std::fmt;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, IntoResponseParts, Response, ResponseParts};
use maud::{Markup, html};

use crate::route::{Endpoint, RouteInfo, Router};

/// The htmx version compiled into the crate.
pub const HTMX_VERSION: &str = "2.0.11";

/// Where [`asset_route`] serves htmx (the version is in the name, so it can be cached forever).
pub const HTMX_PATH: &str = "/assets/htmx-2.0.11.min.js";

/// Subresource-integrity value of [`HTMX_JS`] (the sha384 recorded in `assets/README.md`).
pub const HTMX_INTEGRITY: &str =
  "sha384-2OatzQy1H+Zd/IIrjr1TcuDGqLXeHhbooAyJY1KdQMKnr4LZ22k31GBLdYKHmVjg";

/// The vendored `htmx.min.js` (0BSD), byte for byte.
pub const HTMX_JS: &[u8] = include_bytes!("../../assets/htmx-2.0.11.min.js");

/// The `htmx-config` the layouts set: `eval` off (`allowEval`), `<script>` in swapped content
/// not run (`allowScriptTags`), requests to this origin only (`selfRequestsOnly`), no inline
/// indicator `<style>` (keeps a strict `style-src`), no page snapshots in `localStorage`
/// (`historyCacheSize: 0`), and `422` responses swapped like `2xx` so a re-rendered form with its
/// errors replaces the submitted one; other `4xx`/`5xx` stay errors.
pub const HTMX_CONFIG: &str = concat!(
  r#"{"allowEval":false,"allowScriptTags":false,"selfRequestsOnly":true,"#,
  r#""includeIndicatorStyles":false,"historyCacheSize":0,"#,
  r#""responseHandling":[{"code":"204","swap":false},{"code":"[23]..","swap":true},"#,
  r#"{"code":"422","swap":true},{"code":"[45]..","swap":false,"error":true}]}"#
);

/// `HX-Request` request header (`true` on every htmx request).
pub const HX_REQUEST: &str = "hx-request";
/// `HX-Boosted` request header (`true` for an `hx-boost` navigation).
pub const HX_BOOSTED: &str = "hx-boosted";
/// `HX-History-Restore-Request` request header (`true` when htmx restores a page it has no
/// snapshot of).
pub const HX_HISTORY_RESTORE_REQUEST: &str = "hx-history-restore-request";
/// `HX-Target` request header: the `id` of the target element.
pub const HX_TARGET: &str = "hx-target";
/// `HX-Trigger` header: the triggering element's `id` on a request, client events on a response.
pub const HX_TRIGGER: &str = "hx-trigger";
/// `HX-Trigger-Name` request header: the triggering element's `name`.
pub const HX_TRIGGER_NAME: &str = "hx-trigger-name";
/// `HX-Current-URL` request header: the browser's current URL.
pub const HX_CURRENT_URL: &str = "hx-current-url";
/// `HX-Trigger-After-Settle` response header.
pub const HX_TRIGGER_AFTER_SETTLE: &str = "hx-trigger-after-settle";
/// `HX-Trigger-After-Swap` response header.
pub const HX_TRIGGER_AFTER_SWAP: &str = "hx-trigger-after-swap";
/// `HX-Redirect` response header: a client-side full-page redirect.
pub const HX_REDIRECT: &str = "hx-redirect";
/// `HX-Reswap` response header: override the element's `hx-swap`.
pub const HX_RESWAP: &str = "hx-reswap";
/// `HX-Retarget` response header: a CSS selector replacing the element's `hx-target`.
pub const HX_RETARGET: &str = "hx-retarget";

/// Longest header value [`HxRequest`] keeps (a longer one is dropped as absent).
const MAX_HEADER_TEXT: usize = 2048;

fn flag(headers: &HeaderMap, name: &str) -> bool {
  headers
    .get(name)
    .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"true"))
}

fn text(headers: &HeaderMap, name: &str) -> Option<String> {
  headers
    .get(name)
    .filter(|v| v.len() <= MAX_HEADER_TEXT)
    .and_then(|v| v.to_str().ok())
    .map(str::to_owned)
}

/// The `HX-*` headers of a request. Never rejects: a plain browser request is
/// `HxRequest::default()` (`is_htmx() == false`).
///
/// Header values are client input: use them to choose *what* to render, never render them
/// unescaped (maud escapes them anyway) or trust them for access decisions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HxRequest {
  /// `HX-Request: true`.
  pub htmx: bool,
  /// `HX-Boosted: true`.
  pub boosted: bool,
  /// `HX-History-Restore-Request: true`.
  pub history_restore: bool,
  /// `HX-Target`.
  pub target: Option<String>,
  /// `HX-Trigger` (the triggering element's `id`).
  pub trigger: Option<String>,
  /// `HX-Trigger-Name`.
  pub trigger_name: Option<String>,
  /// `HX-Current-URL`.
  pub current_url: Option<String>,
}

impl HxRequest {
  /// Read the `HX-*` headers.
  pub fn from_headers(headers: &HeaderMap) -> Self {
    Self {
      htmx: flag(headers, HX_REQUEST),
      boosted: flag(headers, HX_BOOSTED),
      history_restore: flag(headers, HX_HISTORY_RESTORE_REQUEST),
      target: text(headers, HX_TARGET),
      trigger: text(headers, HX_TRIGGER),
      trigger_name: text(headers, HX_TRIGGER_NAME),
      current_url: text(headers, HX_CURRENT_URL),
    }
  }

  /// Whether htmx sent the request.
  pub fn is_htmx(&self) -> bool {
    self.htmx
  }

  /// Whether the response should be a fragment rather than the full page: an htmx request that
  /// is neither a boosted navigation nor a history restore (both swap the whole `<body>` and
  /// need the page).
  pub fn wants_fragment(&self) -> bool {
    self.htmx && !self.boosted && !self.history_restore
  }
}

impl<S: Send + Sync> FromRequestParts<S> for HxRequest {
  type Rejection = Infallible;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    Ok(Self::from_headers(&parts.headers))
  }
}

/// The `HX-Target` request header alone (`None` when absent or not an htmx request).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HxTarget(pub Option<String>);

impl<S: Send + Sync> FromRequestParts<S> for HxTarget {
  type Rejection = Infallible;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    Ok(Self(text(&parts.headers, HX_TARGET)))
  }
}

/// A response header value that cannot be sent; answering with it is a `500` (a bug in the
/// handler, not the client's fault).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidHxHeader {
  /// The header (lowercase name).
  pub header: &'static str,
}

impl fmt::Display for InvalidHxHeader {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "invalid value for response header {}", self.header)
  }
}

impl std::error::Error for InvalidHxHeader {}

impl IntoResponse for InvalidHxHeader {
  fn into_response(self) -> Response {
    tracing::error!(header = %self.header, "htmx response header value is not sendable");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
  }
}

fn set(
  mut res: ResponseParts,
  name: &'static str,
  value: &str,
) -> Result<ResponseParts, InvalidHxHeader> {
  let value = HeaderValue::from_str(value).map_err(|_| InvalidHxHeader { header: name })?;
  res
    .headers_mut()
    .insert(HeaderName::from_static(name), value);
  Ok(res)
}

/// When an [`HxTrigger`] fires its events on the client.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TriggerTiming {
  /// As soon as the response arrives (`HX-Trigger`).
  #[default]
  Receive,
  /// After the swap (`HX-Trigger-After-Swap`).
  AfterSwap,
  /// After the settle step (`HX-Trigger-After-Settle`).
  AfterSettle,
}

/// Client-side events for htmx to trigger (`HX-Trigger` and its after-swap / after-settle
/// variants), with optional JSON details.
///
/// Plain names go out as a comma-separated list; once any event has a detail (or a name outside
/// `[A-Za-z0-9_:.-]`) the header is the JSON object form, with non-ASCII text as `\u` escapes so
/// the header stays ASCII.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HxTrigger {
  timing: TriggerTiming,
  events: Vec<(String, Option<serde_json::Value>)>,
}

fn plain_event_name(name: &str) -> bool {
  !name.is_empty()
    && name
      .bytes()
      .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'-'))
}

/// `json` with every non-ASCII character as a `\uXXXX` escape (UTF-16), so the header value is
/// plain ASCII and `JSON.parse` on the client restores the text.
fn ascii_json(json: &str) -> String {
  let mut out = String::with_capacity(json.len());
  for c in json.chars() {
    if c.is_ascii() {
      out.push(c);
    } else {
      let mut units = [0u16; 2];
      for unit in c.encode_utf16(&mut units) {
        out.push_str(&format!("\\u{unit:04x}"));
      }
    }
  }
  out
}

impl HxTrigger {
  /// Trigger the event `name`.
  pub fn event(name: impl Into<String>) -> Self {
    Self::default().and(name)
  }

  /// Also trigger `name`.
  pub fn and(mut self, name: impl Into<String>) -> Self {
    self.events.push((name.into(), None));
    self
  }

  /// Also trigger `name` with `detail` (the event's `detail`).
  pub fn and_detail(mut self, name: impl Into<String>, detail: serde_json::Value) -> Self {
    self.events.push((name.into(), Some(detail)));
    self
  }

  /// Fire after the swap instead of on receipt.
  pub fn after_swap(mut self) -> Self {
    self.timing = TriggerTiming::AfterSwap;
    self
  }

  /// Fire after the settle step instead of on receipt.
  pub fn after_settle(mut self) -> Self {
    self.timing = TriggerTiming::AfterSettle;
    self
  }

  /// The response header this trigger sets.
  pub fn header_name(&self) -> &'static str {
    match self.timing {
      TriggerTiming::Receive => HX_TRIGGER,
      TriggerTiming::AfterSwap => HX_TRIGGER_AFTER_SWAP,
      TriggerTiming::AfterSettle => HX_TRIGGER_AFTER_SETTLE,
    }
  }

  /// The header value: `a, b` or `{"a":null,"b":{…}}`.
  pub fn header_text(&self) -> String {
    let plain = self
      .events
      .iter()
      .all(|(name, detail)| detail.is_none() && plain_event_name(name));
    if plain {
      let names: Vec<&str> = self.events.iter().map(|(n, _)| n.as_str()).collect();
      return names.join(", ");
    }
    let object: serde_json::Map<String, serde_json::Value> = self
      .events
      .iter()
      .map(|(name, detail)| {
        (
          name.clone(),
          detail.clone().unwrap_or(serde_json::Value::Null),
        )
      })
      .collect();
    ascii_json(&serde_json::Value::Object(object).to_string())
  }
}

impl IntoResponseParts for HxTrigger {
  type Error = InvalidHxHeader;

  fn into_response_parts(self, res: ResponseParts) -> Result<ResponseParts, Self::Error> {
    if self.events.is_empty() {
      return Ok(res);
    }
    set(res, self.header_name(), &self.header_text())
  }
}

/// [`HxRedirect::to`] refused a location: it would leave this origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignRedirect {
  /// The refused location.
  pub location: String,
}

impl fmt::Display for ForeignRedirect {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "HX-Redirect refused: {:?} is not a same-origin path",
      self.location
    )
  }
}

impl std::error::Error for ForeignRedirect {}

/// A client-side redirect (`HX-Redirect`) to a path on **this** origin.
///
/// Only an absolute path is accepted (`/todos`, `/a?b=c#d`): no scheme, no authority, no
/// protocol-relative `//host` or `/\host` (browsers read a backslash as a slash), no control
/// characters or whitespace. Anything else is a [`ForeignRedirect`], so request data passed
/// through (a `?next=` parameter) cannot send the user to another site (open redirect, ADR 0013
/// R9).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HxRedirect {
  location: String,
}

impl HxRedirect {
  /// Redirect to `location`, a same-origin absolute path.
  pub fn to(location: impl Into<String>) -> Result<Self, ForeignRedirect> {
    let location = location.into();
    if is_same_origin_path(&location) {
      Ok(Self { location })
    } else {
      Err(ForeignRedirect { location })
    }
  }

  /// The path redirected to.
  pub fn location(&self) -> &str {
    &self.location
  }
}

/// Whether `location` is an absolute path on the current origin.
fn is_same_origin_path(location: &str) -> bool {
  let bytes = location.as_bytes();
  bytes.first() == Some(&b'/')
    && !matches!(bytes.get(1), Some(b'/' | b'\\'))
    && bytes.iter().all(|b| b.is_ascii_graphic())
    && !bytes.contains(&b'\\')
}

impl IntoResponseParts for HxRedirect {
  type Error = InvalidHxHeader;

  fn into_response_parts(self, res: ResponseParts) -> Result<ResponseParts, Self::Error> {
    set(res, HX_REDIRECT, &self.location)
  }
}

/// How htmx swaps the response in (`HX-Reswap`, overriding the element's `hx-swap`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HxReswap {
  /// `innerHTML`: replace the target's children.
  InnerHtml,
  /// `outerHTML`: replace the target itself.
  OuterHtml,
  /// `beforebegin`: insert before the target.
  BeforeBegin,
  /// `afterbegin`: insert before the target's first child.
  AfterBegin,
  /// `beforeend`: append after the target's last child.
  BeforeEnd,
  /// `afterend`: insert after the target.
  AfterEnd,
  /// `delete`: remove the target whatever the response.
  Delete,
  /// `none`: swap nothing (out-of-band swaps still happen).
  None,
}

impl HxReswap {
  /// The `hx-swap` keyword.
  pub fn as_str(self) -> &'static str {
    match self {
      Self::InnerHtml => "innerHTML",
      Self::OuterHtml => "outerHTML",
      Self::BeforeBegin => "beforebegin",
      Self::AfterBegin => "afterbegin",
      Self::BeforeEnd => "beforeend",
      Self::AfterEnd => "afterend",
      Self::Delete => "delete",
      Self::None => "none",
    }
  }
}

impl IntoResponseParts for HxReswap {
  type Error = Infallible;

  fn into_response_parts(self, mut res: ResponseParts) -> Result<ResponseParts, Self::Error> {
    res.headers_mut().insert(
      HeaderName::from_static(HX_RESWAP),
      HeaderValue::from_static(self.as_str()),
    );
    Ok(res)
  }
}

/// Swap into the element matching this CSS selector instead of the element's `hx-target`
/// (`HX-Retarget`). A selector that is not a valid header value answers `500`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HxRetarget(pub String);

impl IntoResponseParts for HxRetarget {
  type Error = InvalidHxHeader;

  fn into_response_parts(self, res: ResponseParts) -> Result<ResponseParts, Self::Error> {
    set(res, HX_RETARGET, &self.0)
  }
}

/// The `<head>` partial loading htmx: `<meta name="htmx-config">` with [`HTMX_CONFIG`] and
/// `<script src=HTMX_PATH integrity=HTMX_INTEGRITY defer>`. The script is same-origin, so a
/// `script-src 'self'` policy admits it without a nonce.
pub fn htmx_head() -> Markup {
  html! {
    meta name="htmx-config" content=(HTMX_CONFIG);
    script src=(HTMX_PATH) integrity=(HTMX_INTEGRITY) defer {}
  }
}

/// A typed router with one public route serving [`HTMX_JS`] at `GET` [`HTMX_PATH`]:
/// `text/javascript`, cached for a year (`immutable`: the version is in the path), `ETag` =
/// [`HTMX_INTEGRITY`] with `304` on a matching `If-None-Match`, `X-Content-Type-Options:
/// nosniff`. [`merge`](crate::route::Router::merge) it into the app's typed router (the route is
/// listed as public in the access report), or mount `asset_route().into_axum()` on a plain
/// [`axum::Router`].
pub fn asset_route<S>() -> Router<S>
where
  S: Clone + Send + Sync + 'static,
{
  let info = RouteInfo::new(Method::GET, HTMX_PATH);
  let endpoint = Endpoint::from_parts(info, axum::routing::get(serve_htmx)).public();
  Router::new().route(endpoint)
}

async fn serve_htmx(headers: HeaderMap) -> Response {
  let etag = concat!(
    "\"",
    "sha384-2OatzQy1H+Zd/IIrjr1TcuDGqLXeHhbooAyJY1KdQMKnr4LZ22k31GBLdYKHmVjg",
    "\""
  );
  let cache = [
    (header::ETAG, HeaderValue::from_static(etag)),
    (
      header::CACHE_CONTROL,
      HeaderValue::from_static("public, max-age=31536000, immutable"),
    ),
    (
      header::X_CONTENT_TYPE_OPTIONS,
      HeaderValue::from_static("nosniff"),
    ),
  ];
  let matches = headers
    .get_all(header::IF_NONE_MATCH)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(','))
    .any(|tag| {
      let tag = tag.trim();
      tag == "*" || tag.trim_start_matches("W/") == etag
    });
  if matches {
    return (StatusCode::NOT_MODIFIED, cache).into_response();
  }
  (
    cache,
    [(
      header::CONTENT_TYPE,
      HeaderValue::from_static("text/javascript; charset=utf-8"),
    )],
    HTMX_JS,
  )
    .into_response()
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::body::Body;
  use axum::http::Request;
  use tower::ServiceExt;

  fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (k, v) in pairs {
      map.insert(
        HeaderName::from_bytes(k.as_bytes()).unwrap(),
        HeaderValue::from_str(v).unwrap(),
      );
    }
    map
  }

  #[test]
  fn hx_request_reads_every_header() {
    let hx = HxRequest::from_headers(&headers(&[
      ("hx-request", "true"),
      ("hx-boosted", "TRUE"),
      ("hx-history-restore-request", "false"),
      ("hx-target", "todos"),
      ("hx-trigger", "add"),
      ("hx-trigger-name", "title"),
      ("hx-current-url", "http://localhost/"),
    ]));
    assert!(hx.is_htmx() && hx.boosted && !hx.history_restore);
    assert_eq!(hx.target.as_deref(), Some("todos"));
    assert_eq!(hx.trigger.as_deref(), Some("add"));
    assert_eq!(hx.trigger_name.as_deref(), Some("title"));
    assert_eq!(hx.current_url.as_deref(), Some("http://localhost/"));
    assert!(!hx.wants_fragment(), "a boosted request needs the page");
    let plain = HxRequest::from_headers(&HeaderMap::new());
    assert_eq!(plain, HxRequest::default());
    assert!(!plain.wants_fragment());
    let fragment = HxRequest::from_headers(&headers(&[("hx-request", "true")]));
    assert!(fragment.wants_fragment());
    let restore = HxRequest::from_headers(&headers(&[
      ("hx-request", "true"),
      ("hx-history-restore-request", "true"),
    ]));
    assert!(!restore.wants_fragment());
    let long = "x".repeat(MAX_HEADER_TEXT + 1);
    let dropped = HxRequest::from_headers(&headers(&[("hx-target", &long)]));
    assert_eq!(dropped.target, None);
  }

  #[tokio::test]
  async fn extractors_never_reject() {
    let (mut parts, ()) = Request::builder()
      .header("hx-request", "true")
      .header("hx-target", "list")
      .body(())
      .unwrap()
      .into_parts();
    let Ok(hx) = HxRequest::from_request_parts(&mut parts, &()).await;
    assert!(hx.is_htmx());
    let Ok(target) = HxTarget::from_request_parts(&mut parts, &()).await;
    assert_eq!(target, HxTarget(Some("list".into())));
  }

  fn parts_of(res: impl IntoResponse) -> Response {
    res.into_response()
  }

  #[test]
  fn trigger_headers_plain_and_json() {
    let res = parts_of((HxTrigger::event("a").and("b:c"), "body"));
    assert_eq!(res.headers()["hx-trigger"], "a, b:c");
    let res = parts_of((
      HxTrigger::event("saved")
        .and_detail("toast", serde_json::json!({"text": "ok"}))
        .after_settle(),
      "",
    ));
    assert_eq!(
      res.headers()["hx-trigger-after-settle"],
      r#"{"saved":null,"toast":{"text":"ok"}}"#
    );
    let odd = HxTrigger::event("has space").after_swap();
    assert_eq!(odd.header_name(), HX_TRIGGER_AFTER_SWAP);
    assert_eq!(odd.header_text(), r#"{"has space":null}"#);
    let res = parts_of((HxTrigger::default(), ""));
    assert!(res.headers().get("hx-trigger").is_none());
    let res = parts_of((HxTrigger::event("é").and_detail("x", "😀".into()), "x"));
    assert_eq!(res.status(), StatusCode::OK);
    let text = res.headers()["hx-trigger"].to_str().unwrap();
    assert!(text.is_ascii(), "{text}");
    for escape in ["u00e9", "ud83d", "ude00"] {
      assert!(text.contains(escape), "{text}");
    }
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["é"], serde_json::Value::Null);
    assert_eq!(parsed["x"], "😀");
  }

  #[test]
  fn redirect_accepts_same_origin_paths_only() {
    for ok in ["/", "/todos", "/a/b?c=d&e=%2F#frag"] {
      let r = HxRedirect::to(ok).unwrap();
      assert_eq!(r.location(), ok);
      let res = parts_of((r, ""));
      assert_eq!(res.headers()["hx-redirect"], ok);
    }
    for bad in [
      "",
      "todos",
      "//evil.example",
      "/\\evil.example",
      "/a\\b",
      "https://evil.example/",
      "javascript:alert(1)",
      "/a b",
      "/a\nb",
    ] {
      let err = HxRedirect::to(bad).unwrap_err();
      assert_eq!(err.location, bad);
      assert!(err.to_string().contains("not a same-origin path"));
    }
  }

  #[test]
  fn reswap_and_retarget_headers() {
    let all = [
      (HxReswap::InnerHtml, "innerHTML"),
      (HxReswap::OuterHtml, "outerHTML"),
      (HxReswap::BeforeBegin, "beforebegin"),
      (HxReswap::AfterBegin, "afterbegin"),
      (HxReswap::BeforeEnd, "beforeend"),
      (HxReswap::AfterEnd, "afterend"),
      (HxReswap::Delete, "delete"),
      (HxReswap::None, "none"),
    ];
    for (swap, text) in all {
      let res = parts_of((swap, HxRetarget("#form".into()), ""));
      assert_eq!(res.headers()["hx-reswap"], text);
      assert_eq!(res.headers()["hx-retarget"], "#form");
    }
    let res = parts_of((HxRetarget("a\nb".into()), ""));
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let err = InvalidHxHeader {
      header: HX_RETARGET,
    };
    assert!(err.to_string().contains("hx-retarget"));
  }

  #[test]
  fn head_partial_loads_the_pinned_script() {
    let head = htmx_head().into_string();
    assert!(head.contains(&format!("src=\"{HTMX_PATH}\"")));
    assert!(head.contains(&format!("integrity=\"{HTMX_INTEGRITY}\"")));
    assert!(head.contains("&quot;allowEval&quot;:false"));
    assert!(HTMX_PATH.contains(HTMX_VERSION));
  }

  #[tokio::test]
  async fn asset_route_serves_and_revalidates() {
    let app: axum::Router = asset_route::<()>().into_axum();
    let res = app
      .clone()
      .oneshot(Request::get(HTMX_PATH).body(Body::empty()).unwrap())
      .await
      .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
      res.headers()["content-type"],
      "text/javascript; charset=utf-8"
    );
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    let etag = res.headers()["etag"].clone();
    assert_eq!(etag.to_str().unwrap(), format!("\"{HTMX_INTEGRITY}\""));
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
      .await
      .unwrap();
    assert_eq!(&body[..], HTMX_JS);
    for tag in [etag.to_str().unwrap().to_owned(), "*".into()] {
      let res = app
        .clone()
        .oneshot(
          Request::get(HTMX_PATH)
            .header("if-none-match", format!("\"other\", {tag}"))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
      assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
    }
  }
}
