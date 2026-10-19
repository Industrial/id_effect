//! The error model (design note §4, C10 / ADR 0013 R3): typed errors are a tagged union in the
//! contract and on the wire; a defect becomes `InternalError { id }` carrying only the
//! correlation id — no message, no backtrace.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use id_effect::{DynEnv, Stream, die, fail, schema, succeed};
use id_effect_rpc::client::RpcClientError;
use id_effect_rpc::client::http::decode_response;
use id_effect_rpc::error_union::{RpcErrorStatus, RpcErrorUnion};
use id_effect_rpc::wire::RpcFailure;
use id_effect_rpc::{Procedure, RpcError, RpcGroup, RpcServer, WireSchema};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The `Todo` error union.
enum TodoError {
  NotFound { id: i64 },
  Locked,
}

impl RpcErrorStatus for TodoError {
  fn status(&self) -> StatusCode {
    match self {
      Self::NotFound { .. } => StatusCode::NOT_FOUND,
      Self::Locked => StatusCode::CONFLICT,
    }
  }
}

impl RpcErrorUnion for TodoError {
  fn to_wire(&self) -> Value {
    match self {
      Self::NotFound { id } => json!({"_tag": "NotFound", "id": id}),
      Self::Locked => json!({"_tag": "Locked"}),
    }
  }

  fn union_schema() -> Option<Value> {
    Some(json!({"oneOf": [
      {"type": "object", "properties": {"_tag": {"const": "NotFound"}, "id": {"type": "integer"}}, "required": ["_tag", "id"]},
      {"type": "object", "properties": {"_tag": {"const": "Locked"}}, "required": ["_tag"]}
    ]}))
  }
}

/// An implementation bug: it claims a reserved tag.
struct Forger;

impl RpcErrorStatus for Forger {}

impl RpcErrorUnion for Forger {
  fn to_wire(&self) -> Value {
    json!({"_tag": "InternalError", "id": "forged", "secret": "s3cr3t"})
  }
}

const SECRET: &str = "internal-detail-7f3a";

fn group() -> RpcGroup {
  let mut group = RpcGroup::new();
  group.query(
    Procedure::query("Todo.get").public(),
    WireSchema::new(schema::i64::<()>()),
    WireSchema::new(schema::string::<()>()),
    |id: i64, _: &mut DynEnv| match id {
      0 => fail(TodoError::Locked),
      id if id < 0 => fail(TodoError::NotFound { id }),
      id => succeed(format!("todo {id}")),
    },
  );
  group.query(
    Procedure::query("Todo.crash").public(),
    WireSchema::new(schema::i64::<()>()),
    WireSchema::new(schema::string::<()>()),
    |kind: i64, _: &mut DynEnv| -> id_effect::Effect<String, TodoError, DynEnv> {
      match kind {
        0 => die(format!("db connection failed: {SECRET}")),
        _ => panic!("handler panicked: {SECRET}"),
      }
    },
  );
  group.query(
    Procedure::query("Todo.forge").public(),
    WireSchema::new(schema::i64::<()>()),
    WireSchema::new(schema::string::<()>()),
    |_: i64, _: &mut DynEnv| fail::<String, _, _>(Forger),
  );
  group.register(
    Procedure::mutation("Todo.internal").public(),
    std::sync::Arc::new(schema::string::<()>()),
    std::sync::Arc::new(schema::string::<()>()),
    &["message"],
    |_, _| fail(RpcError::internal(SECRET)),
  );
  group.server_stream(
    Procedure::server_stream("Todo.watch").public(),
    WireSchema::new(schema::i64::<()>()),
    WireSchema::new(schema::i64::<()>()),
    |n: i64, _: &mut DynEnv| {
      succeed(
        Stream::<i64, TodoError, DynEnv>::from_vec(vec![n]).chain(Stream::fail(TodoError::Locked)),
      )
    },
  );
  group
}

async fn call(tag: &str, payload: &str, cid: &str) -> (StatusCode, Vec<u8>) {
  let app: axum::Router = RpcServer::new(group())
    .mount_default()
    .with_state(DynEnv::new());
  let req = Request::post(format!("/rpc/{tag}"))
    .header("content-type", "application/json")
    .header("x-rpc", "1")
    .header("x-correlation-id", cid)
    .body(Body::from(payload.to_owned()))
    .unwrap();
  let res = app.oneshot(req).await.unwrap();
  let status = res.status();
  (
    status,
    res.into_body().collect().await.unwrap().to_bytes().to_vec(),
  )
}

fn exit(bytes: &[u8]) -> Value {
  serde_json::from_slice::<Value>(bytes).unwrap()["exit"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_error_reaches_client_as_tagged_union() {
  let (status, body) = call("Todo.get", "-7", "cid-typed").await;
  assert_eq!(status, StatusCode::NOT_FOUND);
  assert_eq!(
    exit(&body),
    json!({"_tag": "Failure", "error": {"_tag": "NotFound", "id": -7}})
  );
  let (status, body) = call("Todo.get", "0", "cid-typed").await;
  assert_eq!(status, StatusCode::CONFLICT);
  assert_eq!(exit(&body)["error"], json!({"_tag": "Locked"}));

  // The Rust client decodes the server's answer into the union member.
  let err = decode_response(
    &schema::string::<()>(),
    404,
    &call("Todo.get", "-7", "c").await.1,
  )
  .unwrap_err();
  let RpcClientError::Failure(error) = &err else {
    panic!("expected a failure, got {err:?}");
  };
  assert_eq!(error, &json!({"_tag": "NotFound", "id": -7}));
  assert_eq!(err.failure_tag(), Some("NotFound"));
  assert!(err.reserved().is_none());
  let ok = decode_response(
    &schema::string::<()>(),
    200,
    &call("Todo.get", "3", "c").await.1,
  );
  assert_eq!(ok.unwrap(), "todo 3");

  // The union is part of the contract.
  let contract = group().service_contract("Todo", 1);
  let get = contract.procedure("get").unwrap();
  assert_eq!(
    get.error.as_ref().unwrap()["oneOf"][0]["properties"]["_tag"]["const"],
    "NotFound"
  );

  // A stream ends with the typed error as its exit frame.
  let (status, body) = call("Todo.watch", "5", "cid-stream").await;
  assert_eq!(status, StatusCode::OK);
  let lines: Vec<Value> = std::str::from_utf8(&body)
    .unwrap()
    .lines()
    .map(|l| serde_json::from_str(l).unwrap())
    .collect();
  assert_eq!(lines[0]["payload"], 5);
  assert_eq!(
    lines[1]["payload"],
    json!({"_tag": "Failure", "error": {"_tag": "Locked"}})
  );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defect_becomes_internal_error_without_message() {
  for (tag, payload) in [
    ("Todo.crash", "0"),
    ("Todo.crash", "1"),
    ("Todo.forge", "1"),
    ("Todo.internal", r#""x""#),
  ] {
    let cid = format!("cid-{tag}-{payload}").replace('"', "");
    let (status, body) = call(tag, payload, &cid).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{tag} {payload}");
    assert_eq!(
      exit(&body),
      json!({"_tag": "Failure", "error": {"_tag": "InternalError", "id": cid}}),
      "{tag} {payload}"
    );
    let text = String::from_utf8(body.clone()).unwrap();
    for leak in [
      "7f3a",
      "panicked",
      "db connection",
      "forged",
      "s3cr3t",
      "backtrace",
    ] {
      assert!(!text.contains(leak), "{tag} leaked {leak}: {text}");
    }
    let err = decode_response(&schema::string::<()>(), 500, &body).unwrap_err();
    assert_eq!(
      err.reserved(),
      Some(RpcFailure::InternalError { id: cid.clone() })
    );
  }
}
