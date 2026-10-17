//! The OpenID Connect login against a real identity provider: Keycloak in a container
//! (testcontainers, release spec 0.9.0 D5), with the realm in `fixtures/keycloak/` (client
//! `ledger-web` requiring PKCE S256, user `alice` with realm role `accountant`). An HTTP client
//! plays the browser: it follows the redirects, fills in Keycloak's login form, and ends up
//! logged in to the application (sec.authn #5).
//!
//! Without Docker the test prints `SKIP` and passes, unless `ID_EFFECT_REQUIRE_DOCKER=1` (the CI
//! `containers-integration` job), where a missing Docker daemon fails it.

#![cfg(feature = "oidc")]

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::Extension;
use axum::routing::get;
use id_effect::{Cause, Exit};
use id_effect_auth::oidc::{OidcClient, OidcConfig, OidcRoutes};
use id_effect_auth::{
  AuthPrincipal, Authenticate, ClaimMapping, MemorySessionStore, SessionConfig, Sessions,
};
use id_effect_authz::Principal;
use id_effect_platform::auth::OAuthClient;
use id_effect_tokio::TokioRuntime;
use openidconnect::url::{Url, form_urlencoded};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use serde_json::{Value, json};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt};

const REALM: &[u8] = include_bytes!("fixtures/keycloak/realm-ledger.json");
const CLIENT_ID: &str = "ledger-web";
const CLIENT_SECRET: &str = "ledger-web-secret";

/// Keycloak in dev mode with the `ledger` realm imported, and its base URL; `None` (after
/// printing SKIP) without Docker.
async fn keycloak() -> Option<(ContainerAsync<GenericImage>, String)> {
  let started = GenericImage::new("quay.io/keycloak/keycloak", "26.4")
    .with_exposed_port(8080.tcp())
    .with_wait_for(WaitFor::message_on_either_std("Listening on:"))
    .with_copy_to(
      "/opt/keycloak/data/import/realm-ledger.json",
      REALM.to_vec(),
    )
    .with_cmd(["start-dev", "--import-realm"])
    .with_startup_timeout(Duration::from_secs(300))
    .start()
    .await;
  let container = match started {
    Ok(container) => container,
    Err(e) if std::env::var("ID_EFFECT_REQUIRE_DOCKER").as_deref() == Ok("1") => {
      panic!("ID_EFFECT_REQUIRE_DOCKER=1 but the Keycloak container did not start: {e}")
    }
    Err(e) => {
      eprintln!("SKIP oidc_keycloak: Docker is not available ({e})");
      return None;
    }
  };
  let host = container.get_host().await.expect("host");
  let port = container.get_host_port_ipv4(8080).await.expect("port");
  Some((container, format!("http://{host}:{port}")))
}

/// A browser: no automatic redirects, cookies kept per `host:port` (attributes ignored).
struct Browser {
  http: reqwest::Client,
  jar: HashMap<String, BTreeMap<String, String>>,
}

impl Browser {
  fn new() -> Self {
    Self {
      http: reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .expect("client"),
      jar: HashMap::new(),
    }
  }

  fn origin(url: &Url) -> String {
    format!(
      "{}:{}",
      url.host_str().unwrap(),
      url.port_or_known_default().unwrap()
    )
  }

  fn cookies(&self, url: &Url) -> String {
    self
      .jar
      .get(&Self::origin(url))
      .map(|c| {
        c.iter()
          .map(|(k, v)| format!("{k}={v}"))
          .collect::<Vec<_>>()
          .join("; ")
      })
      .unwrap_or_default()
  }

  fn keep(&mut self, url: &Url, response: &reqwest::Response) {
    let jar = self.jar.entry(Self::origin(url)).or_default();
    for header in response.headers().get_all(SET_COOKIE) {
      let header = header.to_str().unwrap();
      let pair = header.split(';').next().unwrap();
      let (name, value) = pair.split_once('=').unwrap();
      if value.is_empty() || header.contains("Max-Age=0") {
        jar.remove(name);
      } else {
        jar.insert(name.to_owned(), value.to_owned());
      }
    }
  }

  async fn request(&mut self, method: &str, url: &Url, form: Option<String>) -> reqwest::Response {
    let mut request = match method {
      "POST" => self.http.post(url.clone()),
      _ => self.http.get(url.clone()),
    };
    let cookies = self.cookies(url);
    if !cookies.is_empty() {
      request = request.header(COOKIE, cookies);
    }
    if let Some(body) = form {
      request = request
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body);
    }
    let response = request.send().await.expect("request");
    self.keep(url, &response);
    response
  }

  /// `method url`, then every redirect; the final URL and response.
  async fn navigate(
    &mut self,
    method: &str,
    url: &str,
    form: Option<String>,
  ) -> (Url, reqwest::Response) {
    let mut url = Url::parse(url).unwrap();
    let mut response = self.request(method, &url, form).await;
    while response.status().is_redirection() {
      let location = response.headers()[LOCATION].to_str().unwrap().to_owned();
      url = url.join(&location).unwrap();
      response = self.request("GET", &url, None).await;
    }
    (url, response)
  }
}

/// The `action` of Keycloak's login form (`kc-form-login`), HTML-unescaped.
fn login_form_action(html: &str) -> Option<String> {
  let form = &html[html.find("id=\"kc-form-login\"")?..];
  let start = form.find("action=\"")? + "action=\"".len();
  let end = form[start..].find('"')?;
  Some(form[start..start + end].replace("&amp;", "&"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keycloak_login_with_pkce_logs_the_user_in() {
  let begun = Instant::now();
  let Some((_keycloak, base)) = keycloak().await else {
    return;
  };
  eprintln!("keycloak ready in {:?}", begun.elapsed());
  let issuer = format!("{base}/realms/ledger");

  // The application on a real socket.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let app_url = format!("http://{}", listener.local_addr().unwrap());
  let config = OidcConfig::new(
    issuer.clone(),
    CLIENT_ID,
    format!("{app_url}/auth/oidc/callback"),
  )
  .client_secret(CLIENT_SECRET)
  .post_logout_redirect_url(format!("{app_url}/me"))
  .claims(ClaimMapping::default().subject("preferred_username"));
  let client = match TokioRuntime::current()
    .unwrap()
    .run(OidcClient::discover(config))
    .await
  {
    Exit::Success(client) => client,
    Exit::Failure(Cause::Fail(e)) => panic!("discovery: {e}"),
    Exit::Failure(c) => panic!("discovery: {c:?}"),
  };
  assert_eq!(client.issuer(), issuer);
  assert!(client.end_session_endpoint().is_some());
  let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());
  let routes = OidcRoutes::new(client.clone(), sessions.clone()).after_login("/me");
  let app: Router = Router::new()
    .route(
      "/me",
      get(|p: Option<Extension<AuthPrincipal>>| async move {
        axum::Json(match p {
          Some(Extension(p)) => {
            json!({"id": p.id(), "roles": p.roles(), "scheme": p.scheme()})
          }
          None => json!({"id": null}),
        })
      }),
    )
    .merge(routes.router())
    .layer(Authenticate::session(sessions.clone()));
  tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

  // Login: the application redirects to Keycloak, which shows its login form.
  let mut browser = Browser::new();
  let (at, page) = browser
    .navigate("GET", &format!("{app_url}/auth/oidc/login"), None)
    .await;
  assert_eq!(page.status(), StatusCode::OK, "{at}");
  assert!(at.as_str().starts_with(&issuer), "on Keycloak: {at}");
  let query: HashMap<_, _> = at.query_pairs().into_owned().collect();
  assert_eq!(
    query.get("code_challenge_method").map(String::as_str),
    Some("S256")
  );
  let action = login_form_action(&page.text().await.unwrap()).expect("Keycloak login form");

  // The user signs in; Keycloak redirects to the callback, which logs in and lands on /me.
  let form = form_urlencoded::Serializer::new(String::new())
    .append_pair("username", "alice")
    .append_pair("password", "alice-password")
    .append_pair("credentialId", "")
    .finish();
  let (at, me) = browser.navigate("POST", &action, Some(form)).await;
  assert_eq!(at.as_str(), format!("{app_url}/me"));
  assert_eq!(me.status(), StatusCode::OK);
  let me: Value = me.json().await.unwrap();
  assert_eq!(me["id"], "alice", "{me}");
  assert_eq!(
    me["scheme"], "session",
    "later requests ride on the session: {me}"
  );
  assert!(
    me["roles"]
      .as_array()
      .unwrap()
      .contains(&json!("accountant")),
    "realm role mapped: {me}"
  );
  let app_origin = Browser::origin(&Url::parse(&app_url).unwrap());
  assert!(browser.jar[&app_origin].contains_key("__Host-id_session"));
  assert!(
    !browser.jar[&app_origin].contains_key("__Host-id_oidc_flow"),
    "flow consumed"
  );

  // Logout ends the application session and the Keycloak SSO session, then comes back.
  let (at, after) = browser
    .navigate("POST", &format!("{app_url}/auth/oidc/logout"), None)
    .await;
  assert_eq!(at.as_str(), format!("{app_url}/me"));
  assert_eq!(after.json::<Value>().await.unwrap()["id"], Value::Null);
  let (at, page) = browser
    .navigate("GET", &format!("{app_url}/auth/oidc/login"), None)
    .await;
  assert!(at.as_str().starts_with(&issuer), "SSO session ended: {at}");
  assert!(login_form_action(&page.text().await.unwrap()).is_some());

  // Keycloak enforces PKCE for this client: the trait's plain authorization URL is refused.
  let (at, _) = browser
    .navigate("GET", &client.authorization_url("plain-state"), None)
    .await;
  let refused: HashMap<_, _> = at.query_pairs().into_owned().collect();
  assert_eq!(
    refused.get("error").map(String::as_str),
    Some("invalid_request"),
    "{at}"
  );
  eprintln!("keycloak test done in {:?}", begun.elapsed());
}
