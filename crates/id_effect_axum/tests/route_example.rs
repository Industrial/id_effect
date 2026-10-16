//! The `080_typed_routes` example served over a real socket: typed path, query and body decoding,
//! RFC 9457 problems for invalid input (web.validation #2 #3 #7), typed handler failures, and the
//! `/_routes` listing built from the route registry.

use std::net::SocketAddr;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

#[path = "../examples/080_typed_routes.rs"]
#[allow(dead_code)]
mod example;

/// A served example app; dropped at the end of the test (the server task stops with it).
struct Server {
  addr: SocketAddr,
  _stop: oneshot::Sender<()>,
}

async fn serve() -> Server {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let addr = listener.local_addr().unwrap();
  let (stop, stopped) = oneshot::channel::<()>();
  tokio::spawn(async move {
    axum::serve(listener, example::app(example::Store::default()))
      .with_graceful_shutdown(async {
        let _ = stopped.await;
      })
      .await
      .unwrap();
  });
  Server { addr, _stop: stop }
}

/// One HTTP/1.1 request on a fresh connection: status, content type and JSON body.
async fn call(
  server: &Server,
  method: &str,
  target: &str,
  body: Option<Value>,
) -> (u16, String, Value) {
  let body = body.map(|b| b.to_string()).unwrap_or_default();
  let request = format!(
    "{method} {target} HTTP/1.1\r\nhost: {}\r\nconnection: close\r\ncontent-type: \
     application/json\r\ncontent-length: {}\r\n\r\n{body}",
    server.addr,
    body.len()
  );
  let mut stream = TcpStream::connect(server.addr).await.unwrap();
  stream.write_all(request.as_bytes()).await.unwrap();
  let mut raw = Vec::new();
  stream.read_to_end(&mut raw).await.unwrap();
  let raw = String::from_utf8(raw).unwrap();
  let (head, payload) = raw.split_once("\r\n\r\n").expect("response head");
  let mut lines = head.lines();
  let status = lines
    .next()
    .unwrap()
    .split(' ')
    .nth(1)
    .unwrap()
    .parse()
    .unwrap();
  let header = |name: &str| {
    head
      .lines()
      .skip(1)
      .filter_map(|l| l.split_once(':'))
      .find(|(k, _)| k.eq_ignore_ascii_case(name))
      .map(|(_, v)| v.trim().to_owned())
  };
  assert!(header("transfer-encoding").is_none(), "{head}");
  let content_type = header("content-type").unwrap_or_default();
  let value = serde_json::from_str(payload).unwrap_or(Value::Null);
  (status, content_type, value)
}

/// `(in, pointer)` of every issue of a problem body.
fn issues(problem: &Value) -> Vec<(&str, &str)> {
  problem["errors"]
    .as_array()
    .expect("errors")
    .iter()
    .map(|e| (e["in"].as_str().unwrap(), e["pointer"].as_str().unwrap()))
    .collect()
}

#[tokio::test]
async fn accounts_api_decodes_valid_input_over_a_socket() {
  let server = serve().await;
  let ada = json!({ "owner": "ada", "email": "ada@example.com", "opening_balance": 100 });
  let (status, _, body) = call(&server, "POST", "/accounts", Some(ada)).await;
  assert_eq!(status, 201, "{body}");
  assert_eq!(
    body,
    json!({ "id": 1, "owner": "ada", "email": "ada@example.com", "balance": 100 })
  );
  let grace = json!({ "owner": "grace", "email": "grace@example.com", "opening_balance": 0 });
  assert_eq!(call(&server, "POST", "/accounts", Some(grace)).await.0, 201);

  let (status, _, body) = call(&server, "GET", "/accounts?owner=grace", None).await;
  assert_eq!(status, 200);
  assert_eq!(body.as_array().unwrap().len(), 1);
  assert_eq!(body[0]["owner"], "grace");
  let (_, _, body) = call(&server, "GET", "/accounts?limit=1", None).await;
  assert_eq!(body.as_array().unwrap().len(), 1);
  let (_, _, body) = call(&server, "GET", "/accounts", None).await;
  assert_eq!(body.as_array().unwrap().len(), 2);

  let (status, _, body) = call(&server, "GET", "/accounts/2", None).await;
  assert_eq!((status, &body["email"]), (200, &json!("grace@example.com")));
}

#[tokio::test]
async fn invalid_input_is_a_problem_listing_every_issue() {
  let server = serve().await;

  let (status, content_type, body) = call(&server, "GET", "/accounts/0", None).await;
  assert_eq!(
    (status, content_type.as_str()),
    (422, id_effect_axum::PROBLEM_JSON)
  );
  assert_eq!(
    body,
    json!({
      "type": "about:blank",
      "title": "Unprocessable Entity",
      "status": 422,
      "detail": "the request input failed validation (1 issue)",
      "instance": "/accounts/0",
      "errors": [{ "in": "path", "pointer": "", "message": "must be >= 1" }]
    })
  );
  let (status, _, body) = call(&server, "GET", "/accounts/abc", None).await;
  assert_eq!((status, issues(&body)), (422, vec![("path", "")]));

  let (status, content_type, body) = call(&server, "GET", "/accounts?limit=500", None).await;
  assert_eq!(
    (status, content_type.as_str()),
    (422, id_effect_axum::PROBLEM_JSON)
  );
  assert_eq!(issues(&body), vec![("query", "/limit")]);
  assert_eq!(body["errors"][0]["message"], "must be <= 100");

  let bad = json!({ "owner": "", "email": "nope", "opening_balance": -5 });
  let (status, _, body) = call(&server, "POST", "/accounts", Some(bad)).await;
  assert_eq!(status, 422);
  assert_eq!(
    issues(&body),
    vec![
      ("body", "/owner"),
      ("body", "/email"),
      ("body", "/opening_balance")
    ]
  );
  let (status, _, body) = call(&server, "POST", "/accounts", Some(json!({}))).await;
  assert_eq!(status, 422);
  assert_eq!(issues(&body).len(), 3);

  // Nothing was created by the rejected requests.
  let (_, _, body) = call(&server, "GET", "/accounts", None).await;
  assert_eq!(body, json!([]));
}

#[tokio::test]
async fn malformed_json_is_400_and_handler_failures_keep_their_status() {
  let server = serve().await;
  let request = format!(
    "POST /accounts HTTP/1.1\r\nhost: {}\r\nconnection: close\r\ncontent-length: 5\r\n\r\n{{oops",
    server.addr
  );
  let mut stream = TcpStream::connect(server.addr).await.unwrap();
  stream.write_all(request.as_bytes()).await.unwrap();
  let mut raw = String::new();
  stream.read_to_string(&mut raw).await.unwrap();
  assert!(raw.starts_with("HTTP/1.1 400"), "{raw}");
  assert!(raw.contains(id_effect_axum::PROBLEM_JSON), "{raw}");

  let (status, content_type, body) = call(&server, "GET", "/accounts/7", None).await;
  assert_eq!(
    (status, content_type.as_str()),
    (404, id_effect_axum::PROBLEM_JSON)
  );
  assert_eq!(body["detail"], "no account 7");

  let ada = json!({ "owner": "ada", "email": "ada@example.com", "opening_balance": 1 });
  assert_eq!(
    call(&server, "POST", "/accounts", Some(ada.clone()))
      .await
      .0,
    201
  );
  let (status, _, body) = call(&server, "POST", "/accounts", Some(ada)).await;
  assert_eq!((status, issues(&body)), (409, vec![("body", "/email")]));
}

#[tokio::test]
async fn routes_listing_comes_from_the_registry() {
  let server = serve().await;
  let (status, _, body) = call(&server, "GET", "/_routes", None).await;
  assert_eq!(status, 200);
  let listed: Vec<(&str, &str)> = body
    .as_array()
    .unwrap()
    .iter()
    .map(|r| (r["method"].as_str().unwrap(), r["path"].as_str().unwrap()))
    .collect();
  assert_eq!(
    listed,
    vec![
      ("GET", "/accounts"),
      ("POST", "/accounts"),
      ("GET", "/accounts/{id}")
    ]
  );
  assert_eq!(body[0]["query"], "ListAccounts");
  assert_eq!(body[2]["path_params"], "AccountId");
  assert_eq!(
    body[2]["responses"],
    json!([{ "status": 200, "schema": "Account" }])
  );
  let schema = &body[1]["body"];
  assert_eq!(schema["properties"]["email"]["format"], "email");
  assert_eq!(schema["properties"]["opening_balance"]["minimum"], 0);
  assert_eq!(
    schema["required"],
    json!(["owner", "email", "opening_balance"])
  );

  // The listing matches the registry the example builds.
  let registry = example::routes().registry().clone();
  assert_eq!(example::route_listing(&registry), body);
}
