//! Contract testing (feature `contract`): responses of an app checked against the OpenAPI
//! document `id_effect_axum` generates from its typed routes, and every way a response can
//! drift from a hand-written document.

#![cfg(feature = "contract")]

use std::convert::Infallible;

use axum::Json;
use axum::http::StatusCode as AxumStatus;
use axum::routing::get;
use id_effect::schema::{self, HasSchema, Schema, minimum, non_empty};
use id_effect::{Effect, Exit, fail, succeed};
use id_effect_axum::Problem as AxumProblem;
use id_effect_axum::openapi::{Info, OpenApi};
use id_effect_axum::route::{Input, Route, Router};
use id_effect_test::contract::{Contract, ContractError, Violation};
use id_effect_test::http::{Method, StatusCode, TestClient, TestResponse};
use id_effect_tokio::TokioRuntime;
use serde::Serialize;
use serde_json::{Value, json};

/// `{id}`: a positive account number.
struct AccountId;

impl HasSchema for AccountId {
  type A = i64;
  type I = i64;
  type E = ();

  fn schema() -> Schema<i64, i64> {
    minimum(schema::i64(), 1)
  }
}

/// The body of `POST /accounts`.
struct NewAccount;

impl HasSchema for NewAccount {
  type A = (String, i64);
  type I = (String, i64);
  type E = ();

  fn schema() -> Schema<Self::A, Self::I> {
    schema::struct_(
      "owner",
      non_empty(schema::string()),
      "balance",
      minimum(schema::i64(), 0),
    )
  }
}

#[derive(Clone, Debug, PartialEq, Serialize, id_effect::Schema)]
struct Account {
  id: i64,
  owner: String,
  balance: i64,
}

#[derive(Clone, Default)]
struct Store;

fn show(_: &mut Store, input: Input<i64>) -> Effect<Json<Account>, AxumProblem, Store> {
  if input.path == 1 {
    succeed(Json(Account {
      id: 1,
      owner: "ada".into(),
      balance: 100,
    }))
  } else {
    fail(AxumProblem::new(AxumStatus::NOT_FOUND).with_detail("no such account"))
  }
}

fn open(
  _: &mut Store,
  input: Input<(), (), (), (String, i64)>,
) -> Effect<(AxumStatus, Json<Account>), Infallible, Store> {
  let (owner, balance) = input.body;
  succeed((
    AxumStatus::CREATED,
    Json(Account {
      id: 2,
      owner,
      balance,
    }),
  ))
}

fn routes() -> Router<Store> {
  Router::new()
    .route(
      Route::get("/accounts/{id}")
        .path::<AccountId>()
        .response::<Account>(AxumStatus::OK)
        .handler(show),
    )
    .route(
      Route::post("/accounts")
        .body::<NewAccount>()
        .response::<Account>(AxumStatus::CREATED)
        .handler(open),
    )
}

/// The typed app and the contract its own document makes.
fn typed_app() -> (axum::Router, Contract) {
  let router = routes();
  let doc = OpenApi::from_router(&router).info(Info::new("accounts", "1.0.0"));
  let (app, _) = router.with_state::<()>(Store).into_parts();
  (app, Contract::new(doc.to_json()))
}

async fn send<S>(request: id_effect_test::TestRequest<S>) -> TestResponse
where
  S: tower::Service<
      id_effect_test::http::Request<id_effect_test::http::TestBody>,
      Response = axum::response::Response,
      Error = Infallible,
    > + Send
    + 'static,
  S::Future: Send,
{
  match TokioRuntime::current()
    .expect("tokio")
    .run(request.send())
    .await
  {
    Exit::Success(response) => response,
    Exit::Failure(cause) => panic!("request failed: {cause:?}"),
  }
}

#[tokio::test]
async fn responses_of_typed_routes_conform_to_their_generated_document() {
  let (app, contract) = typed_app();
  let client = TestClient::new(app);

  let found = send(client.get("/accounts/1")).await;
  found
    .assert_status(StatusCode::OK)
    .assert_conforms(&contract);
  let created = send(client.post_json("/accounts", &json!({"owner": "bob", "balance": 5}))).await;
  created
    .assert_status(StatusCode::CREATED)
    .assert_conforms(&contract);
  // The documented 422 Problem (a path that does not decode) conforms too.
  let invalid = send(client.get("/accounts/0")).await;
  invalid
    .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
    .assert_conforms(&contract);
  let bad_json = send(client.post("/accounts").body("{", "application/json")).await;
  bad_json
    .assert_status(StatusCode::BAD_REQUEST)
    .assert_conforms(&contract);

  // A status the operation does not document is a contract violation.
  let missing = send(client.get("/accounts/9")).await;
  assert_eq!(missing.status(), StatusCode::NOT_FOUND);
  let Err(ContractError::UndeclaredStatus {
    operation,
    status,
    declared,
  }) = contract.check(&missing)
  else {
    panic!("404 is not documented")
  };
  assert_eq!((operation.as_str(), status), ("GET /accounts/{id}", 404));
  assert_eq!(declared, ["200", "422"]);
}

#[tokio::test]
async fn a_drifted_handler_is_caught_with_json_pointers() {
  let (_, contract) = typed_app();
  // The same path, served by a handler that no longer returns what the document says.
  let drifted = axum::Router::new().route(
    "/accounts/{id}",
    get(|| async { Json(json!({"id": 1, "owner": "ada", "balance": "100"})) }),
  );
  let response = send(TestClient::new(drifted).get("/accounts/1")).await;
  let Err(ContractError::Schema {
    operation,
    status,
    violations,
  }) = contract.check(&response)
  else {
    panic!("the balance is a string")
  };
  assert_eq!((operation.as_str(), status), ("GET /accounts/{id}", 200));
  assert_eq!(
    violations,
    [Violation {
      pointer: "/balance".into(),
      message: r#""100" is not of type "integer""#.into(),
    }]
  );
  let message = contract.check(&response).unwrap_err().to_string();
  assert_eq!(
    message,
    "`GET /accounts/{id}` 200: the body does not match the declared schema\n  at `/balance`: \"100\" is not of type \"integer\""
  );
  let panic = std::panic::catch_unwind(|| {
    response.assert_conforms(&contract);
  })
  .expect_err("assert_conforms panics");
  let text = panic.downcast_ref::<String>().expect("a message");
  assert!(
    text.contains("at `/balance`") && text.contains(r#""balance":"100""#),
    "{text}"
  );
}

/// A hand-written document covering ranges, `default`, `$ref`'d responses and media types.
fn document() -> Value {
  json!({
    "openapi": "3.1.0",
    "info": { "title": "hand-written", "version": "1" },
    "paths": {
      "/items/{id}": {
        "get": { "responses": {
          "200": { "$ref": "#/components/responses/Item" },
          "4XX": { "description": "client error", "content": {
            "application/problem+json": { "schema": { "type": "object", "required": ["title"] } }
          } },
          "default": { "description": "anything else" }
        } },
        "delete": { "responses": { "204": { "description": "gone" } } }
      },
      "/items/latest": {
        "get": { "responses": { "200": { "description": "text", "content": {
          "text/*": { "schema": { "type": "string" } }
        } } } }
      },
      "/broken": {
        "get": { "responses": { "200": { "description": "a dangling ref", "content": {
          "application/json": { "schema": { "$ref": "#/components/schemas/Nope" } }
        } } } }
      }
    },
    "components": {
      "responses": { "Item": { "description": "an item", "content": {
        "application/json": { "schema": { "$ref": "#/components/schemas/Item" } },
        "application/octet-stream": {}
      } } },
      "schemas": { "Item": {
        "type": "object",
        "required": ["id"],
        "properties": { "id": { "type": "integer" }, "tags": { "type": "array", "items": { "type": "string" } } }
      } }
    }
  })
}

/// A router answering every path with `status`, `content_type` and `body`.
fn answering(status: u16, content_type: Option<&'static str>, body: &'static str) -> axum::Router {
  let handler = move || async move {
    let mut response = axum::response::Response::new(axum::body::Body::from(body));
    *response.status_mut() = AxumStatus::from_u16(status).unwrap();
    if let Some(content_type) = content_type {
      response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
      );
    }
    response
  };
  axum::Router::new().fallback(handler)
}

async fn check(
  status: u16,
  content_type: Option<&'static str>,
  body: &'static str,
  method: Method,
  uri: &str,
) -> Result<(), ContractError> {
  let client = TestClient::new(answering(status, content_type, body));
  let response = send(client.request(method, uri)).await;
  Contract::new(document()).check(&response)
}

#[tokio::test]
async fn statuses_media_types_and_bodies_are_checked() {
  let json = Some("application/json; charset=utf-8");
  // A `$ref`'d response; the literal `/items/latest` wins over `/items/{id}`.
  assert_eq!(
    check(
      200,
      json,
      r#"{"id": 1, "tags": ["a"]}"#,
      Method::GET,
      "/items/1?full=1"
    )
    .await,
    Ok(())
  );
  assert_eq!(
    check(
      200,
      Some("text/plain"),
      "hello",
      Method::GET,
      "/items/latest"
    )
    .await,
    Ok(())
  );
  // A declared media type without a schema is not validated.
  assert_eq!(
    check(
      200,
      Some("application/octet-stream"),
      "\x01",
      Method::GET,
      "/items/1"
    )
    .await,
    Ok(())
  );
  // The `4XX` range, and `default` (no content: no body).
  assert_eq!(
    check(
      404,
      Some("application/problem+json"),
      r#"{"title": "Not Found"}"#,
      Method::GET,
      "/items/1"
    )
    .await,
    Ok(())
  );
  assert_eq!(check(503, None, "", Method::GET, "/items/1").await, Ok(()));
  assert_eq!(
    check(204, None, "", Method::DELETE, "/items/1").await,
    Ok(())
  );

  assert_eq!(
    check(200, json, "{}", Method::GET, "/nowhere").await,
    Err(ContractError::UnknownOperation {
      method: "GET".into(),
      path: "/nowhere".into()
    })
  );
  assert_eq!(
    check(200, json, "{}", Method::PUT, "/items/1").await,
    Err(ContractError::UnknownOperation {
      method: "PUT".into(),
      path: "/items/1".into()
    })
  );
  assert_eq!(
    check(500, None, "", Method::DELETE, "/items/1").await,
    Err(ContractError::UndeclaredStatus {
      operation: "DELETE /items/{id}".into(),
      status: 500,
      declared: vec!["204".into()],
    })
  );
  assert_eq!(
    check(204, Some("text/plain"), "x", Method::DELETE, "/items/1").await,
    Err(ContractError::UnexpectedBody {
      operation: "DELETE /items/{id}".into(),
      status: 204
    })
  );
  assert_eq!(
    check(200, json, "", Method::GET, "/items/1").await,
    Err(ContractError::MissingBody {
      operation: "GET /items/{id}".into(),
      status: 200,
      declared: vec!["application/json".into(), "application/octet-stream".into()],
    })
  );
  assert_eq!(
    check(200, Some("text/html"), "<p>", Method::GET, "/items/1").await,
    Err(ContractError::UnexpectedContentType {
      operation: "GET /items/{id}".into(),
      status: 200,
      content_type: "text/html".into(),
      declared: vec!["application/json".into(), "application/octet-stream".into()],
    })
  );
  let Err(ContractError::InvalidJson {
    operation, status, ..
  }) = check(200, json, "{not json", Method::GET, "/items/1").await
  else {
    panic!("not JSON")
  };
  assert_eq!((operation.as_str(), status), ("GET /items/{id}", 200));
  let Err(ContractError::Schema { violations, .. }) = check(
    404,
    Some("application/problem+json"),
    r#"{"detail": "x"}"#,
    Method::GET,
    "/items/1",
  )
  .await
  else {
    panic!("a problem without a title")
  };
  assert_eq!(violations[0].pointer, "");
  assert!(
    violations[0].to_string().starts_with("at the body: "),
    "{}",
    violations[0]
  );
  let Err(ContractError::InvalidDocument { operation, message }) =
    check(200, json, "{}", Method::GET, "/broken").await
  else {
    panic!("a dangling $ref")
  };
  assert_eq!(operation, "GET /broken");
  assert!(message.contains("Nope"), "{message}");
}

#[tokio::test]
async fn an_operation_can_be_named_explicitly() {
  let contract = Contract::new(document());
  assert!(format!("{contract:?}").contains("hand-written"));
  assert_eq!(contract.document()["info"]["version"], "1");
  let client = TestClient::new(answering(200, Some("application/json"), r#"{"id": 7}"#));
  let response = send(client.get("/somewhere/else")).await;
  assert_eq!(
    (response.method(), response.uri().path()),
    (&Method::GET, "/somewhere/else")
  );
  assert!(contract.check(&response).is_err());
  assert_eq!(
    contract.check_operation(&Method::GET, "/items/{id}", &response),
    Ok(())
  );
  assert_eq!(
    contract.check_operation(&Method::GET, "/items/7", &response),
    Ok(())
  );
  // A document without paths knows no operation.
  let empty = Contract::new(json!({"openapi": "3.1.0"}));
  assert!(matches!(
    empty.check(&response),
    Err(ContractError::UnknownOperation { .. })
  ));
}
