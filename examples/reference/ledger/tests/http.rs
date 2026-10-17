//! The HTTP edge: the OpenAPI document and docs page, request ids, compression, the body limit,
//! the request timeout, and `ETag`s on balances.

mod common;

use common::{As, with_ledger};
use id_effect_kit::test::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn openapi_documents_every_typed_route() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let doc: Value = l.get("/openapi.json", &As::Anonymous).await.json().unwrap();
      assert_eq!(doc["openapi"], "3.1.0");
      assert_eq!(doc["info"]["title"], "Ledger");
      let post = &doc["paths"]["/entries"]["post"];
      // The documented body is the decoding schema; guarded routes document 401 / 403.
      assert!(post["requestBody"].is_object(), "{post}");
      assert!(post["responses"]["422"].is_object() && post["responses"]["403"].is_object());
      assert_eq!(post["security"][0]["bearer"], json!(["ledger.post"]));
      let schemes = &doc["components"]["securitySchemes"];
      assert_eq!(schemes["bearer"]["scheme"], "bearer");
      assert_eq!(schemes["session"]["name"], "id_session");
      let account = &doc["components"]["schemas"]["NewAccount"]["properties"];
      assert_eq!(
        account["kind"]["enum"].as_array().unwrap().len(),
        5,
        "{account}"
      );

      let docs = l.get("/docs", &As::Anonymous).await;
      assert_eq!(
        docs.header("content-type"),
        Some("text/html; charset=utf-8")
      );
    },
  )
  .await;
}

#[tokio::test]
async fn request_ids_compression_and_the_body_limit() {
  with_ledger(
    |_, _| {},
    |l| async move {
      // Every response carries a request id; a valid incoming one is kept.
      let res = l
        .send(l.client.get("/health").header("x-request-id", "trace-42"))
        .await;
      assert_eq!(res.header("x-request-id"), Some("trace-42"));
      let fresh = l.get("/health", &As::Anonymous).await;
      assert!(fresh.header("x-request-id").is_some());

      // Large responses are compressed for clients that accept it.
      let zipped = l
        .send(
          l.client
            .get("/openapi.json")
            .header("accept-encoding", "gzip"),
        )
        .await;
      assert_eq!(zipped.header("content-encoding"), Some("gzip"));

      // Bodies over the configured limit are refused before any handler runs.
      let team = l.team("acme").await;
      let As::Session(cookie) = &team.accountant else {
        unreachable!()
      };
      let huge = format!(r#"{{"memo": "{}", "lines": []}}"#, "x".repeat(70_000));
      let res = l
        .send(
          l.client
            .post("/entries")
            .header("cookie", cookie)
            .body(huge, "application/json"),
        )
        .await;
      res.assert_status(StatusCode::PAYLOAD_TOO_LARGE);
      assert_eq!(res.header("content-type"), Some("application/problem+json"));
    },
  )
  .await;
}

#[tokio::test]
async fn slow_requests_time_out() {
  // A 1 ms budget: a login (a database read and an argon2 verification) cannot make it.
  with_ledger(
    |config, _| config.http.request_timeout_ms = 1,
    |l| async move {
      l.org("acme").await;
      l.user("acme", "ada", "owner", None).await;
      let res = l
        .post(
          "/auth/login",
          json!({"username": "ada", "password": common::PASSWORD}),
          &As::Anonymous,
        )
        .await;
      res.assert_status(StatusCode::SERVICE_UNAVAILABLE);
      assert_eq!(res.header("content-type"), Some("application/problem+json"));
    },
  )
  .await;
}

#[tokio::test]
async fn balances_carry_an_etag() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      l.transfer(&team.accountant, accounts[0].id, accounts[1].id, 42)
        .await;
      let path = format!("/accounts/{}/balance", accounts[0].id);
      let As::Session(cookie) = &team.viewer else {
        unreachable!()
      };

      let first = l.get(&path, &team.viewer).await;
      first.assert_status(StatusCode::OK);
      assert_eq!(first.json::<Value>().unwrap()["balance"], 42);
      let etag = first.header("etag").expect("an etag").to_owned();
      let again = l
        .send(
          l.client
            .get(&path)
            .header("cookie", cookie)
            .header("if-none-match", &etag),
        )
        .await;
      again.assert_status(StatusCode::NOT_MODIFIED);

      // A posting changes the balance, so the old tag no longer matches.
      l.transfer(&team.accountant, accounts[0].id, accounts[1].id, 8)
        .await;
      let changed = l
        .send(
          l.client
            .get(&path)
            .header("cookie", cookie)
            .header("if-none-match", &etag),
        )
        .await;
      changed.assert_status(StatusCode::OK);
      assert_eq!(changed.json::<Value>().unwrap()["balance"], 50);
    },
  )
  .await;
}
