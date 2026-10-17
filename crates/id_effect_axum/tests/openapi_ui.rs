//! The `085_openapi` example served over a real socket: `/openapi.json` (content type, `ETag`,
//! `304`), the Scalar `/docs` page and its CSP (also behind `SecurityLayer::recommended`), and the
//! `/_routes` table as JSON and text (data.jsonschema #2 #3 #4, web.server #8).
#![cfg(feature = "openapi")]

use std::collections::HashMap;
use std::net::SocketAddr;

use id_effect::schema::json_schema_for;
use id_effect_axum::openapi::{DEFAULT_SCALAR_URL, OpenApi, OpenApiUi, openapi_router};
use id_effect_axum::server::security::{CookieKey, SecurityLayer};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

#[path = "../examples/085_openapi.rs"]
#[allow(dead_code)]
mod example;

/// A served app; dropped at the end of the test (the server task stops with it).
struct Server {
  addr: SocketAddr,
  _stop: oneshot::Sender<()>,
}

async fn serve(app: axum::Router) -> Server {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let (stop, stopped) = oneshot::channel::<()>();
  tokio::spawn(async move {
    axum::serve(listener, app)
      .with_graceful_shutdown(async {
        let _ = stopped.await;
      })
      .await
      .unwrap();
  });
  Server { addr, _stop: stop }
}

struct Reply {
  status: u16,
  headers: HashMap<String, String>,
  body: String,
}

impl Reply {
  fn header(&self, name: &str) -> &str {
    self.headers.get(name).map_or("", String::as_str)
  }

  fn json(&self) -> Value {
    serde_json::from_str(&self.body).unwrap()
  }
}

/// `GET target` on a fresh HTTP/1.1 connection with extra request header lines.
async fn get(server: &Server, target: &str, extra: &[(&str, &str)]) -> Reply {
  let mut request = format!(
    "GET {target} HTTP/1.1\r\nhost: {}\r\nconnection: close\r\n",
    server.addr
  );
  for (name, value) in extra {
    request.push_str(&format!("{name}: {value}\r\n"));
  }
  request.push_str("\r\n");
  let mut stream = TcpStream::connect(server.addr).await.unwrap();
  stream.write_all(request.as_bytes()).await.unwrap();
  let mut raw = Vec::new();
  stream.read_to_end(&mut raw).await.unwrap();
  let raw = String::from_utf8(raw).unwrap();
  let (head, body) = raw.split_once("\r\n\r\n").expect("response head");
  let mut lines = head.lines();
  let status = lines
    .next()
    .unwrap()
    .split(' ')
    .nth(1)
    .unwrap()
    .parse()
    .unwrap();
  let headers = lines
    .filter_map(|l| l.split_once(':'))
    .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
    .collect();
  Reply {
    status,
    headers,
    body: body.to_owned(),
  }
}

#[tokio::test]
async fn openapi_json_is_served_with_validators() {
  let server = serve(example::app(example::Store::default())).await;
  let reply = get(&server, "/openapi.json", &[]).await;
  assert_eq!(reply.status, 200);
  assert_eq!(reply.header("content-type"), "application/json");
  assert_eq!(reply.header("cache-control"), "no-cache");
  let doc = reply.json();
  assert_eq!(doc["openapi"], "3.1.0");
  assert_eq!(doc, example::document(&example::routes()).to_json());
  // The component schemas are the decoding schemas (data.jsonschema #4).
  assert_eq!(
    doc["components"]["schemas"]["NewAccount"],
    json_schema_for::<example::NewAccount>()
  );
  assert_eq!(
    doc["paths"]["/accounts/{id}"]["get"]["parameters"][0]["name"],
    "id"
  );

  let etag = reply.header("etag").to_owned();
  assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
  let again = get(&server, "/openapi.json", &[("if-none-match", &etag)]).await;
  assert_eq!((again.status, again.body.as_str()), (304, ""));
  assert_eq!(again.header("etag"), etag);
  let weak = format!("\"other\", W/{etag}");
  assert_eq!(
    get(&server, "/openapi.json", &[("if-none-match", &weak)])
      .await
      .status,
    304
  );
  let stale = get(&server, "/openapi.json", &[("if-none-match", "\"stale\"")]).await;
  assert_eq!(stale.status, 200);
}

#[tokio::test]
async fn docs_page_loads_scalar_under_a_strict_policy() {
  let server = serve(example::app(example::Store::default())).await;
  let reply = get(&server, "/docs", &[]).await;
  assert_eq!(reply.status, 200);
  assert_eq!(reply.header("content-type"), "text/html; charset=utf-8");
  assert_eq!(reply.header("x-content-type-options"), "nosniff");
  assert!(reply.body.contains("<title>Ledger</title>"));
  assert!(
    reply
      .body
      .contains(&format!("<script src=\"{DEFAULT_SCALAR_URL}\"></script>"))
  );
  assert!(
    reply
      .body
      .contains("Scalar.createApiReference(\"#app\", {\"url\":\"/openapi.json\"")
  );
  let csp = reply.header("content-security-policy");
  assert_eq!(
    csp,
    OpenApiUi::scalar()
      .content_security_policy()
      .to_header_value()
      .unwrap()
  );
  assert!(csp.starts_with("default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-"));
  assert!(!csp.contains("script-src 'unsafe-inline'"));
}

#[tokio::test]
async fn routes_table_lists_method_path_handler_and_schemas() {
  let server = serve(example::app(example::Store::default())).await;
  let reply = get(&server, "/_routes", &[]).await;
  assert_eq!(reply.status, 200);
  assert_eq!(reply.header("content-type"), "application/json");
  assert_eq!(reply.header("cache-control"), "no-store");
  let table = reply.json();
  let rows: Vec<(&str, &str)> = table
    .as_array()
    .unwrap()
    .iter()
    .map(|r| (r["method"].as_str().unwrap(), r["path"].as_str().unwrap()))
    .collect();
  assert_eq!(
    rows,
    [
      ("GET", "/accounts"),
      ("POST", "/accounts"),
      ("GET", "/accounts/{id}")
    ]
  );
  let create = &table[1];
  assert!(
    create["handler"]
      .as_str()
      .unwrap()
      .ends_with("example::create_account"),
    "{create}"
  );
  assert_eq!(create["body"], "NewAccount");
  assert_eq!(create["body_kind"], "json");
  assert_eq!(
    create["responses"],
    json!([{ "status": 201, "schema": "Account" }])
  );
  assert_eq!(table[0]["query"], "ListAccounts");
  assert_eq!(table[2]["path_params"], "AccountId");
  assert_eq!(table[2]["policies"], json!([]));

  for (target, accept) in [("/_routes?format=text", "*/*"), ("/_routes", "text/plain")] {
    let text = get(&server, target, &[("accept", accept)]).await;
    assert_eq!(text.header("content-type"), "text/plain; charset=utf-8");
    let lines: Vec<&str> = text.body.lines().collect();
    assert_eq!(lines.len(), 4, "{}", text.body);
    assert!(lines[0].starts_with("METHOD  PATH"));
    assert!(lines[2].starts_with("POST    /accounts"));
    assert!(lines[2].contains("body=NewAccount"));
    assert!(lines[2].contains("201 Account"));
  }
}

#[tokio::test]
async fn nothing_is_mounted_unless_merged() {
  let (api, _) = example::routes()
    .with_state::<()>(example::Store::default())
    .into_parts();
  let server = serve(api).await;
  for target in ["/openapi.json", "/docs", "/_routes"] {
    assert_eq!(get(&server, target, &[]).await.status, 404, "{target}");
  }
  assert_eq!(get(&server, "/accounts", &[]).await.status, 200);
}

#[tokio::test]
async fn docs_keep_their_policy_behind_the_security_layer() {
  let app = example::app(example::Store::default())
    .layer(SecurityLayer::recommended(CookieKey::generate()));
  let server = serve(app).await;
  let docs = get(&server, "/docs", &[]).await;
  assert_eq!(docs.status, 200);
  assert!(
    docs
      .header("content-security-policy")
      .starts_with("default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-")
  );
  assert_eq!(docs.header("x-frame-options"), "DENY");
  // Every other route keeps the app's nonce policy.
  let api = get(&server, "/accounts", &[]).await;
  assert_eq!(api.status, 200);
  assert!(
    api.header("content-security-policy").contains("'nonce-"),
    "{}",
    api.header("content-security-policy")
  );
}

#[tokio::test]
async fn a_mirrored_bundle_under_custom_paths() {
  let doc = OpenApi::new().title("Mirror").version("2.0.0");
  let ui = OpenApiUi::scalar()
    .cdn_url("/assets/scalar.js")
    .integrity("sha384-test")
    .title("API reference")
    .json_path("/spec.json")
    .docs_path("/reference")
    .spec_url("/api/spec.json")
    .connect_src(&["https://api.example"]);
  let app = axum::Router::new().nest("/api", openapi_router(&doc, ui));
  let server = serve(app).await;

  let spec = get(&server, "/api/spec.json", &[]).await;
  assert_eq!(spec.status, 200);
  assert_eq!(spec.json()["info"]["version"], "2.0.0");
  let page = get(&server, "/api/reference", &[]).await;
  assert_eq!(page.status, 200);
  assert!(page.body.contains("<title>API reference</title>"));
  assert!(page.body.contains(
    "<script src=\"/assets/scalar.js\" integrity=\"sha384-test\" crossorigin=\"anonymous\">"
  ));
  assert!(page.body.contains("{\"url\":\"/api/spec.json\""));
  let csp = page.header("content-security-policy");
  assert!(csp.contains("script-src 'self' 'sha256-"), "{csp}");
  assert!(csp.contains("style-src 'self' 'unsafe-inline'"), "{csp}");
  assert!(
    csp.contains("connect-src 'self' https://api.example"),
    "{csp}"
  );

  // `OpenApi::routes` is the default UI.
  let server = serve(doc.routes()).await;
  assert_eq!(get(&server, "/openapi.json", &[]).await.status, 200);
  assert!(
    get(&server, "/docs", &[])
      .await
      .body
      .contains("<title>Mirror</title>")
  );
}
