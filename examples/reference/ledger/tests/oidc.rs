//! OpenID Connect login against Keycloak in a container (realm `keycloak/realm-ledger.json`,
//! client `ledger-web` requiring PKCE S256, user `alice` with realm role `accountant` in the
//! organisation `acme`). An HTTP client plays the browser: it follows the redirects, fills in
//! Keycloak's login form and lands on `/me` signed in, then uses the ledger with that session.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use common::{run, with_ledger};
use id_effect::HNil;
use id_effect_kit::auth::{ClaimMapping, OidcClient, OidcConfig};
use id_effect_kit::test::{ContainerError, ContainerImage, Keycloak};
use reqwest::header::{CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use reqwest::{StatusCode, Url};
use serde_json::{Value, json};

const REALM: &str = include_str!("../keycloak/realm-ledger.json");

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

  async fn request(&mut self, method: &str, url: &Url, form: Option<String>) -> reqwest::Response {
    let mut request = match method {
      "POST" => self.http.post(url.clone()),
      _ => self.http.get(url.clone()),
    };
    if let Some(cookies) = self.jar.get(&Self::origin(url)) {
      let header: Vec<String> = cookies.iter().map(|(k, v)| format!("{k}={v}")).collect();
      request = request.header(COOKIE, header.join("; "));
    }
    if let Some(body) = form {
      request = request
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body);
    }
    let response = request.send().await.expect("request");
    let jar = self.jar.entry(Self::origin(url)).or_default();
    for header in response.headers().get_all(SET_COOKIE) {
      let header = header.to_str().unwrap();
      let (name, value) = header.split(';').next().unwrap().split_once('=').unwrap();
      if value.is_empty() || header.contains("Max-Age=0") {
        jar.remove(name);
      } else {
        jar.insert(name.to_owned(), value.to_owned());
      }
    }
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

/// The `action` of Keycloak's login form, HTML-unescaped.
fn login_form_action(html: &str) -> Option<String> {
  let form = &html[html.find("id=\"kc-form-login\"")?..];
  let start = form.find("action=\"")? + "action=\"".len();
  let end = form[start..].find('"')?;
  Some(form[start..start + end].replace("&amp;", "&"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oidc_login_through_keycloak_opens_the_ledger() {
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let keycloak = run(
    Keycloak::new()
      .import_realm(REALM)
      .start::<ContainerError, HNil>(),
  )
  .await;
  let issuer = keycloak.issuer("ledger");

  // The app listens on a real socket: the browser is redirected back to it.
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let app_url = format!("http://{}", listener.local_addr().unwrap());
  let oidc = OidcConfig::new(
    issuer.clone(),
    "ledger-web",
    format!("{app_url}/auth/oidc/callback"),
  )
  .client_secret("ledger-web-secret")
  .claims(ClaimMapping::default().subject("preferred_username"));
  let client = run(OidcClient::discover(oidc)).await;

  with_ledger(
    move |_, services| services.oidc = Some(client),
    move |l| async move {
      l.org("acme").await;
      let app = l.client.service().clone();
      tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

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
      let action = login_form_action(&page.text().await.unwrap()).expect("Keycloak's login form");

      let form = "username=alice&password=alice-password&credentialId=".to_owned();
      let (_, landed) = browser.navigate("POST", &action, Some(form)).await;
      assert_eq!(landed.status(), StatusCode::OK);
      let me: Value = serde_json::from_str(&landed.text().await.unwrap()).unwrap();
      assert_eq!(
        (me["id"].clone(), me["org"].clone()),
        (json!("alice"), json!("acme")),
        "{me}"
      );
      assert!(
        me["roles"]
          .as_array()
          .unwrap()
          .contains(&json!("accountant")),
        "{me}"
      );

      // The session works like a password session: alice opens an account (echoing the CSRF
      // token, like every unsafe request of a browser).
      let opened = browser
        .http
        .post(format!("{app_url}/accounts"))
        .header(
          COOKIE,
          format!(
            "id_session={}; {}",
            browser.jar[&Browser::origin(&Url::parse(&app_url).unwrap())]["id_session"],
            l.csrf.cookie
          ),
        )
        .header(common::CSRF_HEADER, &l.csrf.token)
        .header(CONTENT_TYPE, "application/json")
        .body(r#"{"code": "1000", "name": "Cash", "kind": "asset"}"#)
        .send()
        .await
        .unwrap();
      assert_eq!(opened.status(), StatusCode::CREATED);
      let account: Value = serde_json::from_str(&opened.text().await.unwrap()).unwrap();
      assert_eq!(account["owner_id"], "alice");
    },
  )
  .await;
  drop(keycloak);
}
