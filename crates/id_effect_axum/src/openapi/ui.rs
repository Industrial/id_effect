//! Serving the document (feature **`openapi`**): `GET /openapi.json` and an interactive
//! `GET /docs` page, mounted only where you merge [`openapi_router`] (or [`OpenApi::routes`]).
//!
//! - **`/openapi.json`** is the document as `application/json`, serialized once when the router is
//!   built, with `Cache-Control: no-cache` and a strong `ETag` (a SHA-256 of the body): clients
//!   revalidate on each use and a matching `If-None-Match` answers `304 Not Modified`.
//! - **`/docs`** is a [Scalar](https://github.com/scalar/scalar) API reference page. The page is
//!   a few lines of HTML that load Scalar's standalone bundle from a CDN
//!   ([`DEFAULT_SCALAR_URL`], an exact version, checked by Subresource Integrity against
//!   [`DEFAULT_SCALAR_INTEGRITY`]); no JavaScript is vendored in this crate. For an air-gapped
//!   deployment, mirror the bundle and point [`OpenApiUi::cdn_url`] at the mirror (an absolute URL,
//!   or a same-origin path such as `/assets/scalar.js` served by [`serve_dir`](crate::serve_dir));
//!   another URL drops the default hash, so give the mirrored file's with [`OpenApiUi::integrity`].
//!
//! # Access (ADR 0013 R7)
//!
//! The document lists every route and the page lets a browser call them: outside the `dev` and
//! `test` profiles ([`OpenApiUi::profile`], default the `ID_EFFECT_PROFILE` variable, else
//! `dev`) [`OpenApiUi::router`] mounts **neither route** unless the UI is guarded
//! ([`OpenApiUi::guard`], any [`RouteGuard`] such as an `id_effect_authz` `Guard`) or explicitly
//! public ([`OpenApiUi::public_dangerous`], recorded in the
//! [posture registry](id_effect::observability::posture)). A guard applies in every profile.
//!
//! # Content-Security-Policy
//!
//! The `/docs` response carries its own strict policy ([`OpenApiUi::content_security_policy`]).
//! [`SecurityLayer`](crate::SecurityLayer) keeps a header the handler set, so the page works
//! behind `SecurityLayer::recommended` unchanged while every other route keeps the app's policy:
//!
//! ```text
//! default-src 'none'; script-src <bundle origin> 'sha256-<init script>';
//! style-src <bundle origin> 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:;
//! connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'
//! ```
//!
//! - `script-src` allows the bundle's origin (`'self'` for a same-origin mirror) and exactly the
//!   one inline script that starts the reference, by hash — no `'unsafe-inline'`, no
//!   `'unsafe-eval'`.
//! - `style-src 'unsafe-inline'` is required by Scalar: it renders inline `style="…"` attributes,
//!   which no nonce or hash can authorize. It allows styles only, never scripts.
//! - `connect-src 'self'` lets the page fetch `/openapi.json` and its "try it" client call the API
//!   on the same origin; add other API origins with [`OpenApiUi::connect_src`].
//! - Scalar's web fonts are switched off (`withDefaultFonts: false`), so no font CDN is contacted.
//!
//! ```rust
//! use axum::body::Body;
//! use axum::http::{Request, StatusCode, header};
//! use id_effect_axum::openapi::{OpenApi, OpenApiUi, openapi_router};
//! use tower::ServiceExt;
//!
//! let doc = OpenApi::new().title("Ledger").version("1.0.0");
//! let app: axum::Router = openapi_router(
//!   &doc,
//!   OpenApiUi::scalar().cdn_url("https://mirror.internal/scalar/api-reference.js"),
//! );
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! let get = |uri: &str| Request::get(uri).body(Body::empty()).unwrap();
//! let spec = app.clone().oneshot(get("/openapi.json")).await.unwrap();
//! assert_eq!(spec.headers()[header::CONTENT_TYPE], "application/json");
//! let docs = app.oneshot(get("/docs")).await.unwrap();
//! assert_eq!(docs.status(), StatusCode::OK);
//! let csp = docs.headers()[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
//! assert!(csp.contains("script-src https://mirror.internal 'sha256-"));
//! # });
//! ```

use std::fmt;
use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use super::OpenApi;
use crate::route::RouteGuard;
use crate::server::security::ContentSecurityPolicy;

/// Default Scalar bundle: the standalone build of `@scalar/api-reference` 1.75.0 on jsDelivr,
/// an exact version so that [`DEFAULT_SCALAR_INTEGRITY`] stays its hash.
pub const DEFAULT_SCALAR_URL: &str =
  "https://cdn.jsdelivr.net/npm/@scalar/api-reference@1.75.0/dist/browser/standalone.js";

/// Subresource Integrity hash of [`DEFAULT_SCALAR_URL`]: the browser refuses a bundle the CDN
/// serves with any other content.
pub const DEFAULT_SCALAR_INTEGRITY: &str =
  "sha384-xgelKlDcc+BYp0O+ZYzFesLnWCgGpEbXVVZWNQT6WyZYygN01tbEMygnjPXb5OPI";

/// The variable naming the profile the process runs under (as in `id_effect_config`), read by
/// [`OpenApiUi::router`] when no [`OpenApiUi::profile`] is set.
pub const PROFILE_ENV: &str = "ID_EFFECT_PROFILE";

/// The profiles in which an unguarded UI is mounted.
const OPEN_PROFILES: [&str; 2] = ["dev", "test"];

/// Where [`openapi_router`] serves the document by default.
pub const OPENAPI_JSON_PATH: &str = "/openapi.json";

/// Where [`openapi_router`] serves the docs page by default.
pub const DOCS_PATH: &str = "/docs";

/// How [`openapi_router`] serves the document and its docs page; start with
/// [`OpenApiUi::scalar`].
///
/// Two values are equal when every setting is and they hold the same guards (by identity).
#[derive(Clone)]
#[must_use]
pub struct OpenApiUi {
  bundle_url: String,
  integrity: Option<String>,
  title: Option<String>,
  json_path: String,
  docs_path: String,
  spec_url: Option<String>,
  connect_src: Vec<String>,
  profile: Option<String>,
  guards: Vec<Arc<dyn RouteGuard>>,
  public: bool,
}

impl Default for OpenApiUi {
  fn default() -> Self {
    Self::scalar()
  }
}

impl fmt::Debug for OpenApiUi {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let policies: Vec<&str> = self.guards.iter().map(|guard| guard.policy()).collect();
    f.debug_struct("OpenApiUi")
      .field("bundle_url", &self.bundle_url)
      .field("integrity", &self.integrity)
      .field("title", &self.title)
      .field("json_path", &self.json_path)
      .field("docs_path", &self.docs_path)
      .field("spec_url", &self.spec_url)
      .field("connect_src", &self.connect_src)
      .field("profile", &self.profile)
      .field("guards", &policies)
      .field("public", &self.public)
      .finish()
  }
}

impl PartialEq for OpenApiUi {
  fn eq(&self, other: &Self) -> bool {
    self.bundle_url == other.bundle_url
      && self.integrity == other.integrity
      && self.title == other.title
      && self.json_path == other.json_path
      && self.docs_path == other.docs_path
      && self.spec_url == other.spec_url
      && self.connect_src == other.connect_src
      && self.profile == other.profile
      && self.public == other.public
      && self.guards.len() == other.guards.len()
      && self
        .guards
        .iter()
        .zip(&other.guards)
        .all(|(a, b)| Arc::ptr_eq(a, b))
  }
}

impl Eq for OpenApiUi {}

impl OpenApiUi {
  /// A Scalar API reference at [`DOCS_PATH`] reading the document at [`OPENAPI_JSON_PATH`], its
  /// bundle loaded from [`DEFAULT_SCALAR_URL`] and checked against [`DEFAULT_SCALAR_INTEGRITY`];
  /// unguarded, so mounted in the `dev` and `test` profiles only (see the [module docs](self)).
  pub fn scalar() -> Self {
    Self {
      bundle_url: DEFAULT_SCALAR_URL.to_owned(),
      integrity: Some(DEFAULT_SCALAR_INTEGRITY.to_owned()),
      title: None,
      json_path: OPENAPI_JSON_PATH.to_owned(),
      docs_path: DOCS_PATH.to_owned(),
      spec_url: None,
      connect_src: Vec::new(),
      profile: None,
      guards: Vec::new(),
      public: false,
    }
  }

  /// Load the Scalar bundle from `url` instead (a mirror for air-gapped deployments, or another
  /// pinned version). An absolute `http(s)` URL allows its origin in the page's CSP; a path allows
  /// `'self'`. Another URL than [`DEFAULT_SCALAR_URL`] drops [`DEFAULT_SCALAR_INTEGRITY`] (a hash
  /// set with [`integrity`](Self::integrity) is kept): give the file's own hash.
  pub fn cdn_url(mut self, url: impl Into<String>) -> Self {
    self.bundle_url = url.into();
    if self.bundle_url != DEFAULT_SCALAR_URL
      && self.integrity.as_deref() == Some(DEFAULT_SCALAR_INTEGRITY)
    {
      self.integrity = None;
    }
    self
  }

  /// Subresource Integrity hash of the bundle (`sha384-…`); the script tag then carries
  /// `integrity` and `crossorigin="anonymous"`. Only meaningful with a pinned bundle URL.
  pub fn integrity(mut self, hash: impl Into<String>) -> Self {
    self.integrity = Some(hash.into());
    self
  }

  /// The profile the app runs under (`dev`, `test`, `prod`, …), deciding whether an unguarded UI
  /// is mounted (default: the [`PROFILE_ENV`] variable when [`router`](Self::router) runs, else
  /// `dev`).
  pub fn profile(mut self, profile: impl Into<String>) -> Self {
    self.profile = Some(profile.into());
    self
  }

  /// Put both routes behind `guard` (repeatable, outermost last), in every profile; a guarded UI
  /// is mounted in `prod` too.
  pub fn guard(mut self, guard: impl RouteGuard) -> Self {
    self.guards.push(Arc::new(guard));
    self
  }

  /// Serve the document and the page to anyone in every profile, `prod` included. Recorded in
  /// the [posture registry](id_effect::observability::posture) as
  /// `id_effect_axum::openapi::OpenApiUi::public_dangerous`.
  pub fn public_dangerous(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      "id_effect_axum::openapi::OpenApiUi::public_dangerous",
      "M7: the API document and its reference page are served to anonymous callers in prod",
    );
    self.public = true;
    self
  }

  /// Whether [`router`](Self::router) mounts the routes: guarded, explicitly public, or in the
  /// `dev` or `test` profile.
  pub fn is_mounted(&self) -> bool {
    if self.public || !self.guards.is_empty() {
      return true;
    }
    let profile = self
      .profile
      .clone()
      .or_else(|| std::env::var(PROFILE_ENV).ok())
      .filter(|p| !p.trim().is_empty());
    profile.is_none_or(|p| OPEN_PROFILES.contains(&p.trim()))
  }

  /// Page title (default: the document's `info.title`).
  pub fn title(mut self, title: impl Into<String>) -> Self {
    self.title = Some(title.into());
    self
  }

  /// Route path of the JSON document (default [`OPENAPI_JSON_PATH`]).
  pub fn json_path(mut self, path: impl Into<String>) -> Self {
    self.json_path = path.into();
    self
  }

  /// Route path of the docs page (default [`DOCS_PATH`]).
  pub fn docs_path(mut self, path: impl Into<String>) -> Self {
    self.docs_path = path.into();
    self
  }

  /// URL the docs page fetches the document from (default: the JSON route path). Set it when the
  /// router is nested (`/api/openapi.json` for a router nested at `/api`).
  pub fn spec_url(mut self, url: impl Into<String>) -> Self {
    self.spec_url = Some(url.into());
    self
  }

  /// Extra `connect-src` sources for the page's CSP (the origins of API servers on another
  /// origin, for Scalar's "try it" client).
  pub fn connect_src(mut self, sources: &[&str]) -> Self {
    self
      .connect_src
      .extend(sources.iter().map(|s| (*s).to_owned()));
    self
  }

  /// The inline script that starts the reference (its hash is in the CSP).
  fn init_script(&self) -> String {
    let url = self.spec_url.as_deref().unwrap_or(&self.json_path);
    let config = serde_json::json!({ "url": url, "withDefaultFonts": false }).to_string();
    // `<`, `>` and `&` only occur inside JSON strings here; escaping them keeps `</script>` out.
    let config = config
      .replace('<', "\\u003c")
      .replace('>', "\\u003e")
      .replace('&', "\\u0026");
    format!("Scalar.createApiReference(\"#app\", {config});")
  }

  /// The docs page for `doc`.
  pub fn html(&self, doc: &OpenApi) -> String {
    let title = escape_html(self.title.as_deref().unwrap_or(&doc.info.title));
    let src = escape_html(&self.bundle_url);
    let integrity = self.integrity.as_deref().map_or_else(String::new, |hash| {
      format!(
        " integrity=\"{}\" crossorigin=\"anonymous\"",
        escape_html(hash)
      )
    });
    format!(
      "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta \
       name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n\
       </head>\n<body>\n<div id=\"app\"></div>\n<script \
       src=\"{src}\"{integrity}></script>\n<script>{}</script>\n</body>\n</html>\n",
      self.init_script()
    )
  }

  /// The Content-Security-Policy of the docs page (see the [module docs](self)).
  pub fn content_security_policy(&self) -> ContentSecurityPolicy {
    let bundle = bundle_source(&self.bundle_url);
    let mut connect = vec!["'self'"];
    connect.extend(self.connect_src.iter().map(String::as_str));
    ContentSecurityPolicy::new()
      .default_src(&["'none'"])
      .script_src(&[bundle.as_str()])
      .script_hash(&self.init_script())
      .style_src(&[bundle.as_str(), "'unsafe-inline'"])
      .img_src(&["'self'", "data:"])
      .font_src(&["'self'", "data:"])
      .connect_src(&connect)
      .base_uri(&["'none'"])
      .form_action(&["'none'"])
      .frame_ancestors(&["'none'"])
  }

  /// An Axum router serving `doc` at the JSON path and the docs page at the docs path (see the
  /// [module docs](self)), behind the [guards](Self::guard). The document is serialized now:
  /// later changes to `doc` are not served. An empty router when the UI is not
  /// [mounted](Self::is_mounted) (unguarded outside `dev` and `test`).
  ///
  /// # Panics
  ///
  /// When the bundle URL or a [`connect_src`](Self::connect_src) source cannot be written into a
  /// CSP header (whitespace, `;` or `,`), or a path is not a valid Axum route path.
  pub fn router<S>(&self, doc: &OpenApi) -> axum::Router<S>
  where
    S: Clone + Send + Sync + 'static,
  {
    if !self.is_mounted() {
      tracing::info!(
        target: "id_effect_axum::openapi",
        json = %self.json_path,
        docs = %self.docs_path,
        "OpenAPI document and docs page not mounted: unguarded outside the dev and test profiles",
      );
      return axum::Router::new();
    }
    let policy = self.content_security_policy();
    let csp = match policy.validate() {
      Ok(()) => policy
        .to_header_value()
        .expect("a valid policy renders to a header value"),
      Err(e) => panic!("OpenApiUi: {e}"),
    };
    let json = Bytes::from(serde_json::to_vec(doc).expect("an OpenAPI document serializes"));
    let etag = HeaderValue::from_str(&format!(
      "\"{}\"",
      URL_SAFE_NO_PAD.encode(Sha256::digest(&json))
    ))
    .expect("base64 is a valid header value");
    let html = Bytes::from(self.html(doc));
    let routes = axum::Router::new()
      .route(
        &self.json_path,
        axum::routing::get(move |headers: HeaderMap| {
          std::future::ready(document(&headers, &json, &etag))
        }),
      )
      .route(
        &self.docs_path,
        axum::routing::get(move || std::future::ready(page(html.clone(), csp.clone()))),
      );
    self.guards.iter().fold(routes, |routes, guard| {
      crate::metrics::guard_routes(routes, Arc::clone(guard))
    })
  }
}

/// Serve `doc` at [`OPENAPI_JSON_PATH`] and its docs page as configured by `ui` (see the
/// [module docs](self)); merge the result into your app — nothing is mounted otherwise.
///
/// # Panics
///
/// See [`OpenApiUi::router`].
pub fn openapi_router<S>(doc: &OpenApi, ui: OpenApiUi) -> axum::Router<S>
where
  S: Clone + Send + Sync + 'static,
{
  ui.router(doc)
}

impl OpenApi {
  /// `/openapi.json` and a Scalar `/docs` page for this document
  /// (`openapi_router(self, OpenApiUi::scalar())`).
  pub fn routes<S>(&self) -> axum::Router<S>
  where
    S: Clone + Send + Sync + 'static,
  {
    openapi_router(self, OpenApiUi::scalar())
  }
}

fn document(headers: &HeaderMap, json: &Bytes, etag: &HeaderValue) -> Response {
  let cache = [
    (header::ETAG, etag.clone()),
    (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
  ];
  if if_none_match(headers, etag) {
    return (StatusCode::NOT_MODIFIED, cache).into_response();
  }
  let content_type = [(
    header::CONTENT_TYPE,
    HeaderValue::from_static("application/json"),
  )];
  (content_type, cache, json.clone()).into_response()
}

/// `If-None-Match` lists `etag` (weak comparison) or is `*`.
fn if_none_match(headers: &HeaderMap, etag: &HeaderValue) -> bool {
  let Ok(etag) = etag.to_str() else {
    return false;
  };
  headers
    .get_all(header::IF_NONE_MATCH)
    .iter()
    .filter_map(|v| v.to_str().ok())
    .flat_map(|v| v.split(','))
    .map(str::trim)
    .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == etag)
}

fn page(html: Bytes, csp: HeaderValue) -> Response {
  (
    [
      (
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
      ),
      (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
      (header::CONTENT_SECURITY_POLICY, csp),
      (
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
      ),
    ],
    html,
  )
    .into_response()
}

/// The CSP source allowing `url`: its origin for an absolute URL (`//host/…` keeps the page's
/// scheme), otherwise `'self'`.
fn bundle_source(url: &str) -> String {
  let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
    ("https://", rest)
  } else if let Some(rest) = url.strip_prefix("http://") {
    ("http://", rest)
  } else if let Some(rest) = url.strip_prefix("//") {
    ("", rest)
  } else {
    return "'self'".to_owned();
  };
  let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
  format!("{scheme}{host}")
}

fn escape_html(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  for c in text.chars() {
    match c {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      '\'' => out.push_str("&#39;"),
      c => out.push(c),
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn bundle_sources_are_origins_or_self() {
    assert_eq!(
      bundle_source(DEFAULT_SCALAR_URL),
      "https://cdn.jsdelivr.net"
    );
    assert_eq!(
      bundle_source("http://mirror:8080/scalar.js?v=1"),
      "http://mirror:8080"
    );
    assert_eq!(bundle_source("//cdn.example/x.js"), "cdn.example");
    assert_eq!(bundle_source("/assets/scalar.js"), "'self'");
  }

  #[test]
  fn html_and_script_are_escaped() {
    let doc = OpenApi::new().title("<Ledger & \"Co\">");
    let ui = OpenApiUi::default()
      .spec_url("/x</script><script>alert(1)")
      .integrity("sha384-abc'")
      .cdn_url("/s.js?a=1&b=2");
    let html = ui.html(&doc);
    assert!(html.contains("<title>&lt;Ledger &amp; &quot;Co&quot;&gt;</title>"));
    assert!(html.contains("src=\"/s.js?a=1&amp;b=2\" integrity=\"sha384-abc&#39;\""));
    assert!(html.contains("\"url\":\"/x\\u003c/script\\u003e\\u003cscript\\u003ealert(1)\""));
    assert_eq!(html.matches("</script>").count(), 2);
    assert!(
      OpenApiUi::scalar()
        .title("Docs")
        .html(&doc)
        .contains("<title>Docs</title>")
    );
  }

  #[test]
  fn policy_hashes_the_init_script_and_adds_connect_sources() {
    let ui = OpenApiUi::scalar().connect_src(&["https://api.example"]);
    let csp = ui.content_security_policy().to_header_value().unwrap();
    let csp = csp.to_str().unwrap();
    assert!(csp.starts_with("default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-"));
    assert!(csp.contains("style-src https://cdn.jsdelivr.net 'unsafe-inline'"));
    assert!(csp.contains("connect-src 'self' https://api.example;"));
    assert!(csp.ends_with("frame-ancestors 'none'"));
    let digest =
      base64::engine::general_purpose::STANDARD.encode(Sha256::digest(ui.init_script().as_bytes()));
    assert!(csp.contains(&digest));
  }

  #[test]
  #[should_panic(expected = "OpenApiUi: invalid Content-Security-Policy directive")]
  fn an_unrenderable_policy_panics_at_mount() {
    let _: axum::Router = OpenApiUi::scalar()
      .connect_src(&["https://a.example; script-src *"])
      .router(&OpenApi::new());
  }

  #[test]
  fn if_none_match_compares_weakly_and_accepts_star() {
    let etag = HeaderValue::from_static("\"abc\"");
    let with = |value: &'static str| {
      let mut headers = HeaderMap::new();
      headers.insert(header::IF_NONE_MATCH, HeaderValue::from_static(value));
      headers
    };
    assert!(!if_none_match(&HeaderMap::new(), &etag));
    assert!(if_none_match(&with("\"x\", W/\"abc\""), &etag));
    assert!(if_none_match(&with("*"), &etag));
    assert!(!if_none_match(&with("\"abcd\""), &etag));
    let bad = HeaderValue::from_bytes(b"\"\xff\"").unwrap();
    assert!(!if_none_match(&with("*"), &bad));
  }
}
