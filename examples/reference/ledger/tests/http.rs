//! The HTTP edge: the OpenAPI document and docs page, responses checked against it (contract
//! tests), request ids, compression, the body limit, the request timeout, and `ETag`s on
//! balances.

mod common;

use common::{As, with_ledger};
use id_effect_kit::test::contract::Contract;
use id_effect_kit::test::http::{Method, StatusCode};
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
async fn responses_conform_to_the_openapi_document() {
  with_ledger(
    |_, _| {},
    |l| async move {
      // The contract is the document the app serves; every response below is checked against
      // the schema it declares for that operation and status.
      let doc: Value = l.get("/openapi.json", &As::Anonymous).await.json().unwrap();
      let contract = Contract::new(doc);
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      l.transfer(&team.accountant, accounts[0].id, accounts[1].id, 42)
        .await;

      let me = l.get("/me", &team.owner).await;
      me.assert_status(StatusCode::OK).assert_conforms(&contract);
      let list = l.get("/accounts", &team.viewer).await;
      list
        .assert_status(StatusCode::OK)
        .assert_conforms(&contract);
      let one = l
        .get(&format!("/accounts/{}", accounts[0].id), &team.viewer)
        .await;
      one.assert_status(StatusCode::OK).assert_conforms(&contract);
      // The check is the schema's: `/me`'s body is not an account.
      let mismatch = contract
        .check_operation(&Method::GET, "/accounts/{id}", &me)
        .unwrap_err();
      assert!(
        mismatch
          .to_string()
          .starts_with("`GET /accounts/{id}` 200: the body does not match the declared schema"),
        "{mismatch}"
      );
      let posted = l
        .post(
          "/entries",
          json!({"memo": "rent", "lines": [
            {"account_id": accounts[0].id, "amount": 10},
            {"account_id": accounts[1].id, "amount": -10},
          ]}),
          &team.accountant,
        )
        .await;
      posted
        .assert_status(StatusCode::CREATED)
        .assert_conforms(&contract);

      // Refusals are documented too: a body that does not decode, a caller without the right.
      let invalid = l
        .post(
          "/entries",
          json!({"memo": "", "lines": []}),
          &team.accountant,
        )
        .await;
      invalid
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY)
        .assert_conforms(&contract);
      let denied = l
        .post(
          "/entries",
          json!({"memo": "rent", "lines": [
            {"account_id": accounts[0].id, "amount": 10},
            {"account_id": accounts[1].id, "amount": -10},
          ]}),
          &team.viewer,
        )
        .await;
      denied
        .assert_status(StatusCode::FORBIDDEN)
        .assert_conforms(&contract);
      let anonymous = l.get("/accounts", &As::Anonymous).await;
      anonymous
        .assert_status(StatusCode::UNAUTHORIZED)
        .assert_conforms(&contract);
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
      let huge = format!(r#"{{"memo": "{}", "lines": []}}"#, "x".repeat(70_000));
      let res = l
        .send(l.by(
          &team.accountant,
          l.client.post("/entries").body(huge, "application/json"),
        ))
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
