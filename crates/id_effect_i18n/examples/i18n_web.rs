//! i18n on the web: `Accept-Language` negotiation with a cookie override, a maud page that
//! calls `t(&localizer, "id", args)`, translated `Schema` validation errors, and the negotiated
//! `CurrentLocale` in an effect handler's environment.
//!
//! ```bash
//! cargo run -p id_effect_i18n --all-features --example i18n_web             # serves :3000
//! cargo run -p id_effect_i18n --all-features --example i18n_web -- --self-test
//! curl -H 'Accept-Language: de-AT, en;q=0.5' localhost:3000/
//! ```

use std::collections::HashMap;
use std::convert::Infallible;

use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Form, Router};
use http_body_util::BodyExt;
use id_effect::schema::{ParseErrors, Schema, Unknown, email, min_length, string};
use id_effect::{DynEnv, Effect, succeed};
use id_effect_axum::{RequestCaps, execute_with_request_caps};
use id_effect_i18n::axum::LocaleLayer;
use id_effect_i18n::{Args, Catalog, CurrentLocale, Locale, Localizer, SchemaMessages, t};
use maud::{DOCTYPE, Markup, html};
use tower::ServiceExt;

const EN: &str = "\
title = Sign up
greeting = Welcome, { $name }!
name = Name
email = Email
submit = Create account
created = Account created for { $name }.
";

const DE: &str = "\
title = Registrieren
greeting = Willkommen, { $name }!
name = Name
email = E-Mail
submit = Konto anlegen
created = Konto für { $name } angelegt.
";

fn catalog() -> Catalog {
  let en = Locale::parse("en").expect("en");
  let de = Locale::parse("de").expect("de");
  Catalog::builder(en.clone())
    .embedded(en, "en/app.ftl", EN)
    .embedded(de, "de/app.ftl", DE)
    .schema_messages()
    .require_complete()
    .build()
    .expect("the catalog is complete")
}

/// The whole page around `body`, as an HTML response.
fn page(l: &Localizer, body: Markup) -> Html<String> {
  let markup = html! {
    (DOCTYPE)
    html lang=(l.locale().as_str()) {
      head { meta charset="utf-8"; title { (t(l, "title", Args::new())) } }
      body { (body) }
    }
  };
  Html(markup.into_string())
}

/// `GET /?name=…`: a greeting (the argument is escaped and bidi-isolated) and the form.
async fn index(l: Localizer, query: axum::extract::Query<HashMap<String, String>>) -> Html<String> {
  let name = query.get("name").cloned().unwrap_or_else(|| "Ada".into());
  page(
    &l,
    html! {
      h1 { (t(&l, "greeting", Args::new().with("name", name))) }
      form method="post" action="/signup" {
        label { (t(&l, "name", Args::new())) input name="name"; }
        label { (t(&l, "email", Args::new())) input name="email" type="email"; }
        button { (t(&l, "submit", Args::new())) }
      }
    },
  )
}

fn field(
  form: &HashMap<String, String>,
  name: &str,
  schema: &Schema<String, String>,
  issues: &mut Vec<id_effect::schema::ParseError>,
) -> String {
  let value = form.get(name).cloned().unwrap_or_default();
  if let Err(errors) = schema.decode_unknown_all(&Unknown::String(value.clone())) {
    issues.extend(errors.prefix(name).issues);
  }
  value
}

/// `POST /signup`: validates the form and answers in the request's language.
async fn signup(
  l: Localizer,
  Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Html<String>) {
  let mut issues = Vec::new();
  let name = field(&form, "name", &min_length(string(), 3), &mut issues);
  field(&form, "email", &email(string()), &mut issues);
  if issues.is_empty() {
    let text = t(&l, "created", Args::new().with("name", name));
    return (StatusCode::CREATED, page(&l, html! { p { (text) } }));
  }
  let errors = l.parse_errors(&ParseErrors::new(issues));
  let body = html! {
    ul.errors {
      @for issue in &errors.issues { li data-field=(issue.path) { (issue.message) } }
    }
  };
  (StatusCode::UNPROCESSABLE_ENTITY, page(&l, body))
}

/// `GET /locale`: an effect handler; the layer put the negotiated `CurrentLocale` in its env.
fn whoami(env: &mut DynEnv) -> Effect<String, Infallible, DynEnv> {
  match env.try_get::<CurrentLocale>() {
    Ok(current) => current.get().map(|locale: Locale| locale.to_string()),
    Err(_) => succeed("none".to_owned()),
  }
}

fn app() -> Router {
  let catalog = catalog();
  let base = DynEnv::new()
    .with(catalog.clone())
    .with(CurrentLocale::new(catalog.default_locale().clone()));
  Router::new()
    .route("/", get(index))
    .route("/signup", post(signup))
    .route(
      "/locale",
      get(|st: State<DynEnv>, caps: RequestCaps| execute_with_request_caps(st, caps, whoami)),
    )
    .with_state(base)
    .layer(LocaleLayer::new(catalog).cookie("lang"))
}

async fn call(request: Request<axum::body::Body>) -> (StatusCode, String, String) {
  let response = app().oneshot(request).await.expect("infallible");
  let status = response.status();
  let language = response
    .headers()
    .get(header::CONTENT_LANGUAGE)
    .and_then(|v| v.to_str().ok())
    .unwrap_or_default()
    .to_owned();
  let body = response
    .into_body()
    .collect()
    .await
    .expect("body")
    .to_bytes();
  (
    status,
    language,
    String::from_utf8_lossy(&body).into_owned(),
  )
}

fn get_with(uri: &str, headers: &[(&str, &str)]) -> Request<axum::body::Body> {
  let mut request = Request::get(uri);
  for (name, value) in headers {
    request = request.header(*name, *value);
  }
  request.body(axum::body::Body::empty()).expect("request")
}

async fn self_test() {
  let (status, language, body) = call(get_with(
    "/?name=%3Cscript%3E",
    &[("accept-language", "fr-CH, de-AT;q=0.9, en;q=0.5")],
  ))
  .await;
  assert_eq!((status, language.as_str()), (StatusCode::OK, "de"));
  assert!(
    body.contains("Willkommen, \u{2068}&lt;script&gt;\u{2069}!"),
    "{body}"
  );
  assert!(!body.contains("<script>"));

  let (_, language, body) = call(get_with(
    "/",
    &[("accept-language", "de"), ("cookie", "lang=en")],
  ))
  .await;
  assert_eq!(language, "en");
  assert!(body.contains("Welcome"), "{body}");

  let (_, language, _) = call(get_with("/", &[("accept-language", "fr, ja")])).await;
  assert_eq!(
    language, "en",
    "unsupported locales fall back to the default"
  );

  let flood = vec!["de"; 40].join(",");
  let (_, language, _) = call(get_with("/", &[("accept-language", flood.as_str())])).await;
  assert_eq!(language, "en", "an over-limit header is ignored");

  let (_, _, body) = call(get_with("/locale", &[("accept-language", "de")])).await;
  assert_eq!(body, "de");

  let signup = Request::post("/signup")
    .header("accept-language", "de")
    .header("content-type", "application/x-www-form-urlencoded")
    .body(axum::body::Body::from("name=Al&email=nope"))
    .expect("request");
  let (status, _, body) = call(signup).await;
  assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
  assert!(
    body.contains("Muss mindestens \u{2068}3\u{2069} Zeichen lang sein."),
    "{body}"
  );
  assert!(
    body.contains("Muss eine gültige E-Mail-Adresse sein."),
    "{body}"
  );
  println!("i18n_web self-test: ok");
}

#[tokio::main]
async fn main() {
  if std::env::args().any(|arg| arg == "--self-test") {
    self_test().await;
    return;
  }
  let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
    .await
    .expect("bind 127.0.0.1:3000");
  println!("listening on http://127.0.0.1:3000");
  axum::serve(listener, app()).await.expect("serve");
}
