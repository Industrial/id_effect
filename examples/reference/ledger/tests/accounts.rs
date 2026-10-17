//! Accounts over typed routes: CRUD, Problem+JSON validation, optimistic locking, soft delete,
//! and the tenant filter (one organisation never sees another's accounts).

mod common;

use common::{As, with_ledger};
use id_effect_kit::sql::params;
use id_effect_kit::test::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn accounts_crud_over_typed_routes() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let me = &team.accountant;

      let opened = l
        .post(
          "/accounts",
          json!({"code": "1000", "name": "Cash", "kind": "asset"}),
          me,
        )
        .await;
      opened.assert_status(StatusCode::CREATED);
      let cash: Value = opened.json().unwrap();
      assert_eq!(cash["owner_id"], "acme-accountant");
      assert_eq!(cash["version"], 1);
      let id = cash["id"].as_i64().unwrap();

      let list: Value = l.get("/accounts", &team.viewer).await.json().unwrap();
      assert_eq!(list["accounts"].as_array().unwrap().len(), 1);
      let shown: Value = l.get(&format!("/accounts/{id}"), me).await.json().unwrap();
      assert_eq!(shown["name"], "Cash");

      // A rename carries the version it was made against, and bumps it.
      let renamed = l
        .patch(
          &format!("/accounts/{id}"),
          json!({"name": "Cash at bank", "version": 1}),
          me,
        )
        .await;
      renamed.assert_status(StatusCode::OK);
      assert_eq!(renamed.json::<Value>().unwrap()["version"], 2);
      // A write against the old version lost the race: 409.
      let stale = l
        .patch(
          &format!("/accounts/{id}"),
          json!({"name": "Petty cash", "version": 1}),
          me,
        )
        .await;
      stale.assert_status(StatusCode::CONFLICT);
      assert!(stale.problem().unwrap().detail.unwrap().contains("reload"));

      // The same code twice in one organisation: 409.
      let twice = l
        .post(
          "/accounts",
          json!({"code": "1000", "name": "Again", "kind": "asset"}),
          me,
        )
        .await;
      twice.assert_status(StatusCode::CONFLICT);

      // Closing is a soft delete: gone from every query, still in the table.
      l.delete(&format!("/accounts/{id}"), me)
        .await
        .assert_status(StatusCode::NO_CONTENT);
      l.get(&format!("/accounts/{id}"), me)
        .await
        .assert_status(StatusCode::NOT_FOUND);
      let list: Value = l.get("/accounts", me).await.json().unwrap();
      assert!(list["accounts"].as_array().unwrap().is_empty());
      let kept = l
        .scalar(
          "SELECT count(*)::int8 FROM accounts WHERE id = $1 AND closed_at IS NOT NULL",
          params![id],
        )
        .await;
      assert_eq!(kept, 1);
    },
  )
  .await;
}

#[tokio::test]
async fn invalid_input_answers_problem_json_with_every_issue() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let res = l
        .post(
          "/accounts",
          json!({"code": "1x", "name": "", "kind": "cash"}),
          &team.accountant,
        )
        .await;
      res.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
      assert_eq!(res.header("content-type"), Some("application/problem+json"));
      let problem = res.problem().unwrap();
      let pointers: Vec<&str> = problem.extensions["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["pointer"].as_str().unwrap())
        .collect();
      assert_eq!(pointers, ["/code", "/name", "/kind"], "{problem:?}");

      let bad_id = l.get("/accounts/0", &team.accountant).await;
      bad_id.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
      assert_eq!(
        bad_id.problem().unwrap().extensions["errors"][0]["in"],
        "path"
      );

      let not_json = l
        .send(
          l.client
            .post("/accounts")
            .header(
              "cookie",
              match &team.accountant {
                As::Session(c) => c,
                _ => unreachable!(),
              },
            )
            .body("{not json", "application/json"),
        )
        .await;
      not_json.assert_status(StatusCode::BAD_REQUEST);
    },
  )
  .await;
}

#[tokio::test]
async fn organisations_never_see_each_other() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let acme = l.team("acme").await;
      let globex = l.team("globex").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (cash, revenue) = (accounts[0].id, accounts[1].id);

      // The tenant filter: another organisation's account does not exist for globex.
      let list: Value = l.get("/accounts", &globex.owner).await.json().unwrap();
      assert!(list["accounts"].as_array().unwrap().is_empty());
      l.get(&format!("/accounts/{cash}"), &globex.owner)
        .await
        .assert_status(StatusCode::NOT_FOUND);
      l.get(&format!("/accounts/{cash}/balance"), &globex.owner)
        .await
        .assert_status(StatusCode::NOT_FOUND);
      let posting = l
        .post(
          "/entries",
          json!({"memo": "theft", "lines": [
            {"account_id": cash, "amount": -100},
            {"account_id": revenue, "amount": 100},
          ]}),
          &globex.accountant,
        )
        .await;
      posting.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
      // acme still sees its own.
      let list: Value = l.get("/accounts", &acme.viewer).await.json().unwrap();
      assert_eq!(list["accounts"].as_array().unwrap().len(), 2);
    },
  )
  .await;
}
