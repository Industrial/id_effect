//! Per-request locale negotiation for axum (feature `axum`; eco.i18n #3).
//!
//! [`LocaleLayer`] chooses each request's locale with a [`Negotiator`] (application preference,
//! then the locale cookie, then `Accept-Language`, then the default; only allowlisted locales) and
//! records it for the rest of the stack:
//!
//! - a [`Localizer`] in the request extensions, which is also an extractor, for views
//!   (`t(&localizer, "id", args)` with the `maud` feature);
//! - a [`CurrentLocale`] override in the request's
//!   [`RequestCaps`](id_effect_axum::RequestCaps): handlers run through
//!   [`execute_with_request_caps`](id_effect_axum::execute_with_request_caps) find the negotiated
//!   locale in their [`DynEnv`](id_effect::DynEnv) environment; typed environments call
//!   [`Localizer::scope`];
//! - the [`LocaleSource`] that decided, in the request extensions.
//!
//! The response gets `Content-Language` (unless the handler set one) and `Vary: Accept-Language`
//! (plus `Cookie` when the cookie is read), so caches keep one copy per language.
//!
//! ```
//! use axum::{Router, routing::get};
//! use id_effect_i18n::axum::LocaleLayer;
//! use id_effect_i18n::{Args, Catalog, Locale, Localizer};
//!
//! let en = Locale::parse("en").unwrap();
//! let de = Locale::parse("de").unwrap();
//! let catalog = Catalog::builder(en.clone())
//!   .embedded(en, "en.ftl", "hello = Hello!")
//!   .embedded(de, "de.ftl", "hello = Hallo!")
//!   .build()
//!   .unwrap();
//!
//! async fn hello(localizer: Localizer) -> String {
//!   localizer.format("hello", &Args::new())
//! }
//!
//! let app: Router = Router::new()
//!   .route("/", get(hello))
//!   .layer(LocaleLayer::new(catalog).cookie("lang"));
//! # let _ = app;
//! ```

use std::convert::Infallible;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use ::axum::extract::FromRequestParts;
use ::axum::http::header::{ACCEPT_LANGUAGE, CONTENT_LANGUAGE, COOKIE, VARY};
use ::axum::http::request::Parts;
use ::axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use ::axum::response::IntoResponse;
use id_effect_axum::RequestCaps;
use tower_layer::Layer;
use tower_service::Service;

use crate::negotiate::{LocaleSource, Localizer, Negotiator};
use crate::{Catalog, CurrentLocale, Locale};

/// The cookie [`LocaleLayer`] reads unless configured otherwise.
pub const DEFAULT_LOCALE_COOKIE: &str = "locale";

type Preference = Arc<dyn Fn(&Parts) -> Option<String> + Send + Sync>;

#[derive(Clone)]
struct Config {
  catalog: Catalog,
  negotiator: Negotiator,
  cookie: Option<String>,
  preference: Option<Preference>,
}

/// A Tower layer that negotiates each request's locale; see the [module docs](self).
#[derive(Clone)]
pub struct LocaleLayer {
  config: Arc<Config>,
}

impl fmt::Debug for LocaleLayer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("LocaleLayer")
      .field("supported", &self.config.negotiator.supported())
      .field("cookie", &self.config.cookie)
      .field("preference", &self.config.preference.is_some())
      .finish()
  }
}

impl LocaleLayer {
  /// Negotiates among the locales of `catalog` (its default as the fallback) and reads the
  /// [`DEFAULT_LOCALE_COOKIE`] cookie.
  pub fn new(catalog: Catalog) -> Self {
    let negotiator = Negotiator::from_catalog(&catalog);
    Self::with_negotiator(catalog, negotiator)
  }

  /// Negotiates with `negotiator` (for example a narrower allowlist than the catalog's locales)
  /// and translates from `catalog`.
  pub fn with_negotiator(catalog: Catalog, negotiator: Negotiator) -> Self {
    Self {
      config: Arc::new(Config {
        catalog,
        negotiator,
        cookie: Some(DEFAULT_LOCALE_COOKIE.to_owned()),
        preference: None,
      }),
    }
  }

  fn map_config(self, f: impl FnOnce(&mut Config)) -> Self {
    let mut config = Arc::unwrap_or_clone(self.config);
    f(&mut config);
    Self {
      config: Arc::new(config),
    }
  }

  /// Reads the locale override from the cookie `name` (a language switcher sets it).
  #[must_use]
  pub fn cookie(self, name: impl Into<String>) -> Self {
    let name = name.into();
    self.map_config(|c| c.cookie = Some(name))
  }

  /// Ignores cookies: only the preference and `Accept-Language` decide.
  #[must_use]
  pub fn without_cookie(self) -> Self {
    self.map_config(|c| c.cookie = None)
  }

  /// Reads the application's preference for a request (a stored user setting, typically from an
  /// extension an authentication layer inserted before this one). It beats the cookie and the
  /// header, and like them is used only when it matches a supported locale.
  #[must_use]
  pub fn preference(
    self,
    resolve: impl Fn(&Parts) -> Option<String> + Send + Sync + 'static,
  ) -> Self {
    let resolve: Preference = Arc::new(resolve);
    self.map_config(|c| c.preference = Some(resolve))
  }

  /// The negotiator deciding among the supported locales.
  pub fn negotiator(&self) -> &Negotiator {
    &self.config.negotiator
  }

  /// The locale this layer chooses for a request with these `parts`, and where it came from.
  pub fn select(&self, parts: &Parts) -> (Locale, LocaleSource) {
    self.config.select(parts)
  }
}

impl Config {
  fn select(&self, parts: &Parts) -> (Locale, LocaleSource) {
    let preference = self.preference.as_ref().and_then(|resolve| resolve(parts));
    let cookie = self
      .cookie
      .as_deref()
      .and_then(|name| cookie_value(&parts.headers, name));
    let accept_language = accept_language(&parts.headers);
    self
      .negotiator
      .select(preference.as_deref(), cookie, accept_language.as_deref())
  }
}

/// The value of cookie `name`, from any `Cookie` header.
fn cookie_value<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
  headers
    .get_all(COOKIE)
    .iter()
    .filter_map(|value| value.to_str().ok())
    .flat_map(|value| value.split(';'))
    .find_map(|pair| {
      let (key, value) = pair.trim().split_once('=')?;
      (key == name).then(|| value.trim_matches('"'))
    })
}

/// The `Accept-Language` value; several header lines are joined with `,` (RFC 9110 §5.3), and
/// the joined value is subject to the parser's limits. A value that is not visible ASCII is
/// ignored.
fn accept_language(headers: &HeaderMap) -> Option<String> {
  let mut joined: Option<String> = None;
  for value in headers.get_all(ACCEPT_LANGUAGE) {
    let value = value.to_str().ok()?;
    match joined.as_mut() {
      Some(all) => {
        all.push(',');
        all.push_str(value);
      }
      None => joined = Some(value.to_owned()),
    }
  }
  joined
}

impl<S> Layer<S> for LocaleLayer {
  type Service = LocaleService<S>;

  fn layer(&self, inner: S) -> Self::Service {
    LocaleService {
      inner,
      config: Arc::clone(&self.config),
    }
  }
}

/// The service [`LocaleLayer`] produces.
#[derive(Clone)]
pub struct LocaleService<S> {
  inner: S,
  config: Arc<Config>,
}

impl<S> fmt::Debug for LocaleService<S> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("LocaleService").finish_non_exhaustive()
  }
}

impl<S, B, ResBody> Service<Request<B>> for LocaleService<S>
where
  S: Service<Request<B>, Response = Response<ResBody>>,
  S::Future: Send + 'static,
{
  type Response = Response<ResBody>;
  type Error = S::Error;
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.inner.poll_ready(cx)
  }

  fn call(&mut self, req: Request<B>) -> Self::Future {
    let (mut parts, body) = req.into_parts();
    let (locale, source) = self.config.select(&parts);
    let mut caps = parts.extensions.remove::<RequestCaps>().unwrap_or_default();
    caps.insert(CurrentLocale::new(locale.clone()));
    parts.extensions.insert(caps);
    parts.extensions.insert(source);
    parts
      .extensions
      .insert(Localizer::new(self.config.catalog.clone(), locale.clone()));
    let vary = if self.config.cookie.is_some() {
      HeaderValue::from_static("accept-language, cookie")
    } else {
      HeaderValue::from_static("accept-language")
    };
    let future = self.inner.call(Request::from_parts(parts, body));
    Box::pin(async move {
      let mut response = future.await?;
      let headers = response.headers_mut();
      if !headers.contains_key(CONTENT_LANGUAGE)
        && let Ok(value) = HeaderValue::from_str(locale.as_str())
      {
        headers.insert(CONTENT_LANGUAGE, value);
      }
      headers.append(VARY, vary);
      Ok(response)
    })
  }
}

/// The rejection of the [`Localizer`] extractor when no [`LocaleLayer`] ran for the request: a
/// configuration error, answered with `500 Internal Server Error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingLocaleLayer;

impl fmt::Display for MissingLocaleLayer {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("no LocaleLayer ran for this request")
  }
}

impl std::error::Error for MissingLocaleLayer {}

impl IntoResponse for MissingLocaleLayer {
  fn into_response(self) -> ::axum::response::Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
  }
}

/// Extracts the [`Localizer`] the [`LocaleLayer`] negotiated.
impl<S: Send + Sync> FromRequestParts<S> for Localizer {
  type Rejection = MissingLocaleLayer;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    parts
      .extensions
      .get::<Localizer>()
      .cloned()
      .ok_or(MissingLocaleLayer)
  }
}

/// Extracts where the [`LocaleLayer`] found the locale ([`LocaleSource::Default`] without one).
impl<S: Send + Sync> FromRequestParts<S> for LocaleSource {
  type Rejection = Infallible;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    Ok(
      parts
        .extensions
        .get::<LocaleSource>()
        .copied()
        .unwrap_or(LocaleSource::Default),
    )
  }
}
