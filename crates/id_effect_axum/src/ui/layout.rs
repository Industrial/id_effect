//! Layouts and partials: one handler answers the full page to a browser and only the fragment to
//! htmx.
//!
//! A handler returns a [`Page`]: a title and its content (a partial). On its own a `Page` answers
//! the content alone. Behind a [`LayoutLayer`] the response is chosen by the request's
//! `HX-Request` header ([`HxRequest::wants_fragment`]): an htmx request (not boosted, not a
//! history restore) gets the fragment, anything else gets the content wrapped in the
//! [`Layout`]. `Page` responses carry `Vary: HX-Request` so caches keep the two apart. This works
//! the same for typed [`Route`](crate::route::Route) handlers, which never see the request
//! headers, and for plain Axum handlers.
//!
//! The layout receives a [`LayoutContext`]: the `HX-*` headers, this session's [`CsrfToken`] and
//! the CSP nonce, both read from the request extensions a [`SecurityLayer`](crate::SecurityLayer)
//! fills (install it **outside** the `LayoutLayer`, i.e. with a later `.layer` call).
//! [`BasicLayout`] puts the masked token in `<body hx-headers='{"x-csrf-token": …}'>`, so every
//! htmx request carries it (ADR 0013 R9), and loads htmx through
//! [`htmx_head`](super::htmx::htmx_head).
//!
//! ```rust
//! use axum::Router;
//! use axum::body::Body;
//! use axum::http::Request;
//! use id_effect::{Effect, succeed};
//! use id_effect_axum::routing;
//! use id_effect_axum::ui::{BasicLayout, LayoutLayer, Page, html, maud};
//! use tower::ServiceExt;
//!
//! fn inbox(_: &mut ()) -> Effect<Page, std::convert::Infallible, ()> {
//!   succeed(Page::new("Inbox", html! { ul#messages { li { "hello" } } }))
//! }
//!
//! let app: Router = Router::new()
//!   .route("/", routing::get(inbox))
//!   .layer(LayoutLayer::new(BasicLayout::new().site_name("Mail")))
//!   .with_state(());
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let page = app.clone().oneshot(Request::get("/").body(Body::empty()).unwrap()).await.unwrap();
//! let page = axum::body::to_bytes(page.into_body(), usize::MAX).await.unwrap();
//! assert!(page.starts_with(b"<!DOCTYPE html>"));
//!
//! let htmx = Request::get("/").header("hx-request", "true").body(Body::empty()).unwrap();
//! let fragment = app.oneshot(htmx).await.unwrap();
//! let fragment = axum::body::to_bytes(fragment.into_body(), usize::MAX).await.unwrap();
//! assert_eq!(&fragment[..], b"<ul id=\"messages\"><li>hello</li></ul>");
//! # });
//! ```

use std::borrow::Cow;
use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::extract::{FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use maud::{DOCTYPE, Markup, Render, html};
use tower_layer::Layer;
use tower_service::Service;

use super::htmx::{HX_REQUEST, HxRequest, htmx_head};
use crate::server::security::{CSRF_FORM_FIELD, CSRF_HEADER, CspNonce, CsrfToken};

/// A titled page whose content is also its htmx fragment; see the [module docs](self).
#[derive(Clone, Debug)]
pub struct Page {
  title: String,
  content: Markup,
  status: StatusCode,
}

impl Page {
  /// A `200` page titled `title` (escaped) with `content`.
  pub fn new(title: impl Into<String>, content: impl Render) -> Self {
    Self {
      title: title.into(),
      content: content.render(),
      status: StatusCode::OK,
    }
  }

  /// Answer with `status` instead of `200` (e.g. `422` for a form re-rendered with its errors).
  pub fn status(mut self, status: StatusCode) -> Self {
    self.status = status;
    self
  }

  /// The title.
  pub fn title(&self) -> &str {
    &self.title
  }

  /// The content (the fragment).
  pub fn content(&self) -> &Markup {
    &self.content
  }
}

/// What a [`Page`] response leaves for the [`LayoutLayer`].
#[derive(Clone, Debug)]
struct PageParts {
  title: String,
  content: Markup,
}

impl IntoResponse for Page {
  fn into_response(self) -> Response {
    let parts = PageParts {
      title: self.title,
      content: self.content.clone(),
    };
    let mut res = (self.status, self.content).into_response();
    res
      .headers_mut()
      .append(header::VARY, HeaderValue::from_static(HX_REQUEST));
    res.extensions_mut().insert(parts);
    res
  }
}

/// What a [`Layout`] knows about the request: its `HX-*` headers, the session's CSRF token and
/// the CSP nonce (both present behind a [`SecurityLayer`](crate::SecurityLayer)).
///
/// Also an extractor (never rejects) for plain handlers that render with [`respond`].
#[derive(Clone, Default)]
pub struct LayoutContext {
  hx: HxRequest,
  csrf: Option<CsrfToken>,
  nonce: Option<CspNonce>,
}

impl fmt::Debug for LayoutContext {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("LayoutContext")
      .field("hx", &self.hx)
      .field("csrf", &self.csrf)
      .field("nonce", &self.nonce.is_some())
      .finish()
  }
}

impl LayoutContext {
  /// Read the context from a request's headers and extensions.
  pub fn from_parts(headers: &HeaderMap, extensions: &Extensions) -> Self {
    Self {
      hx: HxRequest::from_headers(headers),
      csrf: extensions.get::<CsrfToken>().cloned(),
      nonce: extensions.get::<CspNonce>().cloned(),
    }
  }

  /// Replace the `HX-*` headers (tests, or rendering outside a request).
  pub fn with_hx(mut self, hx: HxRequest) -> Self {
    self.hx = hx;
    self
  }

  /// Use `token` as the session's CSRF token.
  pub fn with_csrf(mut self, token: CsrfToken) -> Self {
    self.csrf = Some(token);
    self
  }

  /// The request's `HX-*` headers.
  pub fn hx(&self) -> &HxRequest {
    &self.hx
  }

  /// The session's CSRF token (masked text), when a CSRF layer issued one.
  pub fn csrf_token(&self) -> Option<&CsrfToken> {
    self.csrf.as_ref()
  }

  /// The request's CSP nonce, when the policy uses one (for an inline `<script nonce>`).
  pub fn nonce(&self) -> Option<&CspNonce> {
    self.nonce.as_ref()
  }

  /// The `hx-headers` JSON carrying the masked CSRF token on every htmx request:
  /// `{"x-csrf-token":"<masked>"}`. `None` without a token.
  pub fn hx_headers(&self) -> Option<String> {
    let token = self.csrf.as_ref()?;
    let mut map = serde_json::Map::new();
    map.insert(CSRF_HEADER.to_owned(), token.as_str().into());
    Some(serde_json::Value::Object(map).to_string())
  }

  /// The hidden `csrf_token` input for a plain `<form method="post">` (empty without a token).
  pub fn csrf_field(&self) -> Markup {
    html! {
      @if let Some(token) = &self.csrf {
        input type="hidden" name=(CSRF_FORM_FIELD) value=(token.as_str());
      }
    }
  }
}

impl<S: Send + Sync> FromRequestParts<S> for LayoutContext {
  type Rejection = Infallible;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    Ok(Self::from_parts(&parts.headers, &parts.extensions))
  }
}

/// The page around a [`Page`]'s content.
///
/// Implemented by [`BasicLayout`] and by any `Fn(&LayoutContext, &str, Markup) -> Markup`.
/// Interpolate through [`html!`](super::html) only: the title and everything else are escaped.
pub trait Layout: Send + Sync + 'static {
  /// The full document for `content` titled `title`.
  fn render(&self, ctx: &LayoutContext, title: &str, content: Markup) -> Markup;
}

impl<F> Layout for F
where
  F: Fn(&LayoutContext, &str, Markup) -> Markup + Send + Sync + 'static,
{
  fn render(&self, ctx: &LayoutContext, title: &str, content: Markup) -> Markup {
    self(ctx, title, content)
  }
}

/// A minimal HTML5 document: `lang`, charset, viewport, `<title>` (with an optional site name),
/// stylesheets, htmx ([`htmx_head`]) and `<body hx-headers>` carrying the CSRF token.
#[derive(Clone, Debug)]
pub struct BasicLayout {
  lang: Cow<'static, str>,
  site_name: Option<String>,
  stylesheets: Vec<String>,
  htmx: bool,
}

impl Default for BasicLayout {
  fn default() -> Self {
    Self::new()
  }
}

impl BasicLayout {
  /// English, no site name, no stylesheets, htmx on.
  pub fn new() -> Self {
    Self {
      lang: Cow::Borrowed("en"),
      site_name: None,
      stylesheets: Vec::new(),
      htmx: true,
    }
  }

  /// The document language (`<html lang>`).
  pub fn lang(mut self, lang: impl Into<Cow<'static, str>>) -> Self {
    self.lang = lang.into();
    self
  }

  /// Append ` · <name>` to every title.
  pub fn site_name(mut self, name: impl Into<String>) -> Self {
    self.site_name = Some(name.into());
    self
  }

  /// Link a stylesheet (same-origin under the default CSP).
  pub fn stylesheet(mut self, href: impl Into<String>) -> Self {
    self.stylesheets.push(href.into());
    self
  }

  /// Leave htmx out (a page without interactive fragments).
  pub fn without_htmx(mut self) -> Self {
    self.htmx = false;
    self
  }
}

impl Layout for BasicLayout {
  fn render(&self, ctx: &LayoutContext, title: &str, content: Markup) -> Markup {
    html! {
      (DOCTYPE)
      html lang=(self.lang) {
        head {
          meta charset="utf-8";
          meta name="viewport" content="width=device-width, initial-scale=1";
          title {
            (title)
            @if let Some(site) = &self.site_name { " · " (site) }
          }
          @for href in &self.stylesheets {
            link rel="stylesheet" href=(href);
          }
          @if self.htmx { (htmx_head()) }
        }
        body hx-headers=[ctx.hx_headers()] {
          (content)
        }
      }
    }
  }
}

/// Answer `page` for the request described by `ctx`: the fragment for an htmx fragment request,
/// otherwise the page inside `layout`. For plain handlers that do not sit behind a
/// [`LayoutLayer`].
pub fn respond<L: Layout + ?Sized>(layout: &L, ctx: &LayoutContext, page: Page) -> Response {
  wrap(layout, ctx, page.into_response())
}

/// Wrap a [`Page`] response in `layout` unless `ctx` asks for the fragment; other responses
/// pass unchanged.
fn wrap<L: Layout + ?Sized>(layout: &L, ctx: &LayoutContext, mut res: Response) -> Response {
  let Some(page) = res.extensions_mut().remove::<PageParts>() else {
    return res;
  };
  if ctx.hx().wants_fragment() {
    return res;
  }
  let document = layout.render(ctx, &page.title, page.content).into_string();
  let (mut head, _fragment) = res.into_parts();
  head.headers.remove(header::CONTENT_LENGTH);
  head.headers.insert(
    header::CONTENT_TYPE,
    HeaderValue::from_static("text/html; charset=utf-8"),
  );
  Response::from_parts(head, Body::from(document))
}

/// Tower layer wrapping every [`Page`] response in a [`Layout`] unless the request is an htmx
/// fragment request; see the [module docs](self).
#[derive(Clone)]
pub struct LayoutLayer {
  layout: Arc<dyn Layout>,
}

impl fmt::Debug for LayoutLayer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("LayoutLayer").finish_non_exhaustive()
  }
}

impl LayoutLayer {
  /// Wrap pages in `layout`.
  pub fn new(layout: impl Layout) -> Self {
    Self {
      layout: Arc::new(layout),
    }
  }
}

impl<S> Layer<S> for LayoutLayer {
  type Service = LayoutService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    LayoutService {
      inner,
      layout: Arc::clone(&self.layout),
    }
  }
}

/// The [`Service`] produced by [`LayoutLayer`].
#[derive(Clone)]
pub struct LayoutService<S> {
  inner: S,
  layout: Arc<dyn Layout>,
}

impl<S> fmt::Debug for LayoutService<S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("LayoutService").finish_non_exhaustive()
  }
}

impl<S> Service<Request> for LayoutService<S>
where
  S: Service<Request, Error = Infallible> + Clone + Send + 'static,
  S::Response: IntoResponse,
  S::Future: Send + 'static,
{
  type Response = Response;
  type Error = Infallible;
  type Future = Pin<Box<dyn Future<Output = Result<Response, Infallible>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, request: Request) -> Self::Future {
    let ctx = LayoutContext::from_parts(request.headers(), request.extensions());
    // The ready service handles this request; the clone waits for the next `poll_ready`.
    let clone = self.inner.clone();
    let mut inner = std::mem::replace(&mut self.inner, clone);
    let layout = Arc::clone(&self.layout);
    Box::pin(async move {
      let res = match inner.call(request).await {
        Ok(res) => res.into_response(),
        Err(never) => match never {},
      };
      Ok(wrap(&*layout, &ctx, res))
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::server::security::{CookieKey, SecurityLayer};
  use axum::Router;
  use tower::ServiceExt;

  async fn text(res: Response) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
      .await
      .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
  }

  #[tokio::test]
  async fn page_alone_answers_the_fragment() {
    let page = Page::new("T", html! { p { "x" } }).status(StatusCode::IM_A_TEAPOT);
    assert_eq!(page.title(), "T");
    assert_eq!(page.content().clone().into_string(), "<p>x</p>");
    let res = page.into_response();
    assert_eq!(res.status(), StatusCode::IM_A_TEAPOT);
    assert_eq!(res.headers()["vary"], "hx-request");
    assert_eq!(text(res).await, "<p>x</p>");
  }

  #[tokio::test]
  async fn respond_picks_page_or_fragment_and_passes_other_responses() {
    let layout = |_: &LayoutContext, title: &str, content: Markup| {
      html! { main data-title=(title) { (content) } }
    };
    let page = || Page::new("<T>", html! { p { "x" } });
    let full = respond(&layout, &LayoutContext::default(), page());
    assert!(full.headers().get("content-length").is_none());
    assert_eq!(
      text(full).await,
      "<main data-title=\"&lt;T&gt;\"><p>x</p></main>"
    );
    let hx = HxRequest {
      htmx: true,
      ..HxRequest::default()
    };
    let ctx = LayoutContext::default().with_hx(hx);
    assert_eq!(text(respond(&layout, &ctx, page())).await, "<p>x</p>");
    let other = wrap(&layout, &LayoutContext::default(), "plain".into_response());
    assert_eq!(text(other).await, "plain");
  }

  #[test]
  fn basic_layout_renders_head_and_csrf_headers() {
    let token = {
      // A token as the CSRF layer issues it, read back through a request.
      let rt = tokio::runtime::Runtime::new().unwrap();
      rt.block_on(async {
        let app: Router = Router::new()
          .route(
            "/",
            axum::routing::get(|ctx: LayoutContext| async move {
              format!("{:?}", ctx.csrf_token().is_some() && ctx.nonce().is_some())
            }),
          )
          .layer(SecurityLayer::recommended(CookieKey::generate()));
        let res = app
          .oneshot(Request::get("/").body(Body::empty()).unwrap())
          .await
          .unwrap();
        text(res).await
      })
    };
    assert_eq!(token, "true");
    let layout = BasicLayout::default()
      .lang("de")
      .site_name("Site")
      .stylesheet("/app.css");
    let html = layout
      .render(&LayoutContext::default(), "Home", html! { "body" })
      .into_string();
    assert!(html.starts_with("<!DOCTYPE html><html lang=\"de\">"));
    assert!(html.contains("<title>Home · Site</title>"));
    assert!(html.contains("<link rel=\"stylesheet\" href=\"/app.css\">"));
    assert!(html.contains("htmx-config"));
    assert!(html.contains("<body>body</body>"), "{html}");
    let bare = BasicLayout::new()
      .without_htmx()
      .render(&LayoutContext::default(), "x", html! {})
      .into_string();
    assert!(!bare.contains("htmx"));
    assert!(LayoutContext::default().hx_headers().is_none());
    assert_eq!(LayoutContext::default().csrf_field().into_string(), "");
    assert!(format!("{:?}", LayoutContext::default()).contains("LayoutContext"));
    assert!(format!("{:?}", LayoutLayer::new(BasicLayout::new())).contains("LayoutLayer"));
  }
}
