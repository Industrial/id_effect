//! i18n on the web (r13-e9-4-i18n-web): the axum `LocaleLayer` (eco.i18n #3) and translated
//! messages in maud views (web.ui #8), with the ADR 0013 R4/R9 defaults.
#![cfg(all(feature = "axum", feature = "maud"))]

use std::convert::Infallible;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{ACCEPT_LANGUAGE, CONTENT_LANGUAGE, COOKIE, VARY};
use axum::http::{HeaderValue, Request, Response, StatusCode};
use axum::response::Html;
use axum::routing::get;
use http_body_util::BodyExt;
use id_effect::{DynEnv, Effect, succeed};
use id_effect_axum::{RequestCaps, execute_with_request_caps};
use id_effect_i18n::axum::{DEFAULT_LOCALE_COOKIE, LocaleLayer, MissingLocaleLayer};
use id_effect_i18n::negotiate::{LocaleSource, Negotiator};
use id_effect_i18n::{Args, Catalog, CurrentLocale, Locale, Localizer, t};
use maud::{Markup, html};
use tower::ServiceExt;

fn loc(tag: &str) -> Locale {
  Locale::parse(tag).expect("test locale")
}

/// en (default), de, ar; bidi isolation left at its default (on).
fn catalog() -> Catalog {
  Catalog::builder(loc("en"))
    .embedded(loc("en"), "en.ftl", "greeting = Hello, { $name }!")
    .embedded(loc("de"), "de.ftl", "greeting = Hallo, { $name }!")
    .embedded(loc("ar"), "ar.ftl", "greeting = مرحبا، { $name }!")
    .build()
    .expect("catalog")
}

async fn greet(l: Localizer, query: axum::extract::Query<Vec<(String, String)>>) -> Html<String> {
  let name = query
    .iter()
    .find_map(|(k, v)| (k == "name").then(|| v.clone()))
    .unwrap_or_else(|| "Ada".into());
  let markup: Markup =
    html! { p lang=(l.locale().as_str()) { (t(&l, "greeting", Args::new().with("name", name))) } };
  Html(markup.into_string())
}

async fn source(source: LocaleSource) -> String {
  format!("{source:?}")
}

/// An effect handler: the layer put the negotiated `CurrentLocale` in its environment.
fn env_locale(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  match env.try_get::<CurrentLocale>() {
    Ok(current) => current.get().map(|locale: Locale| locale.to_string()),
    Err(_) => succeed("none".to_owned()),
  }
}

fn app(layer: LocaleLayer) -> Router {
  let base = DynEnv::new().with(CurrentLocale::new(loc("en")));
  Router::new()
    .route("/", get(greet))
    .route("/source", get(source))
    .route(
      "/env",
      get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, env_locale)),
    )
    .with_state(base)
    .layer(layer)
}

fn request(uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
  let mut builder = Request::get(uri);
  for (name, value) in headers {
    builder = builder.header(*name, *value);
  }
  builder.body(Body::empty()).expect("request")
}

async fn send(app: Router, req: Request<Body>) -> (Response<()>, String) {
  let response = app.oneshot(req).await.expect("infallible");
  let (parts, body) = response.into_parts();
  let bytes = body.collect().await.expect("body").to_bytes();
  (
    Response::from_parts(parts, ()),
    String::from_utf8(bytes.to_vec()).expect("utf-8"),
  )
}

fn language(response: &Response<()>) -> &str {
  response
    .headers()
    .get(CONTENT_LANGUAGE)
    .and_then(|v| v.to_str().ok())
    .unwrap_or_default()
}

#[tokio::test]
async fn cookie_override_beats_header() {
  let app = app(LocaleLayer::new(catalog()));
  let cookie = format!("theme=dark; {DEFAULT_LOCALE_COOKIE}=de");
  let (response, body) = send(
    app.clone(),
    request(
      "/",
      &[("accept-language", "ar, en;q=0.5"), ("cookie", &cookie)],
    ),
  )
  .await;
  assert_eq!(language(&response), "de");
  assert!(body.contains("Hallo"), "{body}");
  let (_, body) = send(
    app.clone(),
    request("/source", &[("accept-language", "ar"), ("cookie", &cookie)]),
  )
  .await;
  assert_eq!(body, "Cookie");

  // Without the cookie the header decides.
  let (response, _) = send(
    app.clone(),
    request("/", &[("accept-language", "ar, en;q=0.5")]),
  )
  .await;
  assert_eq!(language(&response), "ar");

  // A cookie outside the allowlist is ignored; the header decides.
  let (response, _) = send(
    app.clone(),
    request(
      "/",
      &[("accept-language", "de"), ("cookie", "locale=\"fr\"")],
    ),
  )
  .await;
  assert_eq!(language(&response), "de");

  // A custom cookie name; the default one is then not read.
  let app = self::app(LocaleLayer::new(catalog()).cookie("lang"));
  let (response, _) = send(
    app.clone(),
    request("/", &[("accept-language", "de"), ("cookie", "locale=ar")]),
  )
  .await;
  assert_eq!(language(&response), "de");
  let (response, _) = send(
    app,
    request("/", &[("accept-language", "de"), ("cookie", "lang=\"ar\"")]),
  )
  .await;
  assert_eq!(language(&response), "ar");
}

#[tokio::test]
async fn preference_beats_cookie_and_cookie_can_be_disabled() {
  #[derive(Clone)]
  struct UserLanguage(&'static str);

  let layer = LocaleLayer::new(catalog()).preference(|parts| {
    parts
      .extensions
      .get::<UserLanguage>()
      .map(|u| u.0.to_owned())
  });
  let mut req = request(
    "/source",
    &[("cookie", "locale=de"), ("accept-language", "en")],
  );
  req.extensions_mut().insert(UserLanguage("ar"));
  let (response, body) = send(app(layer.clone()), req).await;
  assert_eq!((language(&response), body.as_str()), ("ar", "Preference"));
  let (parts, _body) = request("/", &[("cookie", "locale=de")]).into_parts();
  assert_eq!(layer.select(&parts), (loc("de"), LocaleSource::Cookie));

  let layer = LocaleLayer::new(catalog()).without_cookie();
  let (response, body) = send(
    app(layer),
    request(
      "/source",
      &[("cookie", "locale=de"), ("accept-language", "ar")],
    ),
  )
  .await;
  assert_eq!(
    (language(&response), body.as_str()),
    ("ar", "AcceptLanguage")
  );
  assert_eq!(
    response.headers().get(VARY),
    Some(&HeaderValue::from_static("accept-language"))
  );
}

#[tokio::test]
async fn negotiated_locale_is_in_the_request_environment() {
  let app = app(LocaleLayer::new(catalog()));
  let (_, body) = send(
    app.clone(),
    request("/env", &[("accept-language", "de-CH")]),
  )
  .await;
  assert_eq!(body, "de");
  let (_, body) = send(app, request("/env", &[])).await;
  assert_eq!(body, "en");
}

#[tokio::test]
async fn r4_accept_language_over_limit_ignored_by_the_layer() {
  let app = app(LocaleLayer::new(catalog()));
  let flood = vec!["de"; 33].join(",");
  let (response, _) = send(app.clone(), request("/", &[("accept-language", &flood)])).await;
  assert_eq!(language(&response), "en");
  let long = format!("de;q=0.9,{}", " ".repeat(300));
  let (response, _) = send(app.clone(), request("/", &[("accept-language", &long)])).await;
  assert_eq!(language(&response), "en");

  // Several header lines are one list, and the limits apply to all of them together.
  let mut req = request("/", &[]);
  req
    .headers_mut()
    .append(ACCEPT_LANGUAGE, HeaderValue::from_static("fr"));
  req
    .headers_mut()
    .append(ACCEPT_LANGUAGE, HeaderValue::from_static("de;q=0.5"));
  let (response, _) = send(app.clone(), req).await;
  assert_eq!(language(&response), "de");
  let mut req = request("/", &[]);
  for _ in 0..20 {
    req
      .headers_mut()
      .append(ACCEPT_LANGUAGE, HeaderValue::from_static("fr,ja"));
  }
  req
    .headers_mut()
    .append(ACCEPT_LANGUAGE, HeaderValue::from_static("de"));
  let (response, _) = send(app.clone(), req).await;
  assert_eq!(language(&response), "en");

  // A value that is not visible ASCII is ignored.
  let mut req = request("/", &[]);
  req.headers_mut().insert(
    ACCEPT_LANGUAGE,
    HeaderValue::from_bytes(b"de\xff").expect("opaque value"),
  );
  req.headers_mut().insert(
    COOKIE,
    HeaderValue::from_bytes(b"locale=\xff").expect("opaque"),
  );
  let (response, _) = send(app, req).await;
  assert_eq!(language(&response), "en");
}

#[tokio::test]
async fn locale_outside_allowlist_falls_back_in_the_layer() {
  // A narrower allowlist than the catalog: `ar` is in the catalog but not offered.
  let layer = LocaleLayer::with_negotiator(catalog(), Negotiator::new(loc("en"), [loc("de")]));
  assert_eq!(layer.negotiator().supported(), [loc("en"), loc("de")]);
  let app = app(layer);
  let (response, body) = send(
    app,
    request("/", &[("accept-language", "ar"), ("cookie", "locale=ar")]),
  )
  .await;
  assert_eq!(language(&response), "en");
  assert!(body.contains("Hello"), "{body}");
}

#[tokio::test]
async fn response_headers_and_missing_layer() {
  let app = app(LocaleLayer::new(catalog()));
  let (response, _) = send(app, request("/", &[("accept-language", "de")])).await;
  assert_eq!(
    response.headers().get(VARY),
    Some(&HeaderValue::from_static("accept-language, cookie"))
  );

  // A handler's own Content-Language is kept.
  let app = Router::new()
    .route(
      "/",
      get(|| async { ([(CONTENT_LANGUAGE, "x-custom")], "body") }),
    )
    .layer(LocaleLayer::new(catalog()));
  let (response, _) = send(app, request("/", &[("accept-language", "de")])).await;
  assert_eq!(language(&response), "x-custom");

  // The extractor without the layer is a configuration error: 500, no detail.
  let app = Router::new()
    .route("/", get(greet))
    .route("/source", get(source));
  let (response, body) = send(app.clone(), request("/", &[])).await;
  assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
  assert!(!body.contains("LocaleLayer"));
  let (_, body) = send(app, request("/source", &[])).await;
  assert_eq!(body, "Default");
  assert!(!MissingLocaleLayer.to_string().is_empty());
  let layer = LocaleLayer::new(catalog());
  assert!(format!("{layer:?}").contains("supported"));
  let service = tower::Layer::layer(
    &layer,
    tower::service_fn(|_: Request<Body>| async {
      Ok::<_, Infallible>(Response::new(Body::empty()))
    }),
  );
  assert!(format!("{service:?}").contains("LocaleService"));
}

#[test]
fn i18n_args_escaped_in_html() {
  let l = Localizer::new(catalog(), loc("en"));
  let hostile = "<script>alert(\"x\")</script> & <b>";
  let page = html! { p { (t(&l, "greeting", Args::new().with("name", hostile))) } }.into_string();
  assert!(!page.contains("<script>"), "{page}");
  assert!(!page.contains("<b>"), "{page}");
  assert!(
    page.contains("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt; &amp; &lt;b&gt;"),
    "{page}"
  );
  // The translation itself is text too.
  let markup_in_catalog = Catalog::builder(loc("en"))
    .embedded(loc("en"), "en.ftl", "k = <img src=x onerror=alert(1)>")
    .build()
    .expect("catalog");
  let l = Localizer::new(markup_in_catalog, loc("en"));
  let page = html! { (t(&l, "k", Args::new())) }.into_string();
  assert_eq!(page, "&lt;img src=x onerror=alert(1)&gt;");
}

#[tokio::test]
async fn i18n_args_escaped_in_html_through_the_layer() {
  let app = app(LocaleLayer::new(catalog()));
  let (_, body) = send(
    app,
    request(
      "/?name=%3Cscript%3Ealert(1)%3C%2Fscript%3E",
      &[("accept-language", "de")],
    ),
  )
  .await;
  assert!(!body.contains("<script>"), "{body}");
  assert!(
    body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
    "{body}"
  );
}

#[test]
fn i18n_bidi_isolation_on_by_default() {
  // The catalog was built without `use_isolating`: arguments are isolated (FSI … PDI).
  let l = Localizer::new(catalog(), loc("en"));
  let page = html! { (t(&l, "greeting", Args::new().with("name", "عليّ"))) }.into_string();
  assert_eq!(page, "Hello, \u{2068}عليّ\u{2069}!");
  let ar = Localizer::new(catalog(), loc("ar"));
  let page = html! { (t(&ar, "greeting", Args::new().with("name", "Ada"))) }.into_string();
  assert_eq!(page, "مرحبا، \u{2068}Ada\u{2069}!");
  // Escaping applies inside the isolation marks.
  let page = html! { (t(&l, "greeting", Args::new().with("name", "<i>"))) }.into_string();
  assert_eq!(page, "Hello, \u{2068}&lt;i&gt;\u{2069}!");
}
