//! Authorization: roles owner / accountant / viewer, a guard on every route, the `can_post`
//! policy, ownership checks, and the recorded allow / deny decisions.

mod common;

use common::{As, with_ledger};
use id_effect_kit::authz::Outcome;
use id_effect_kit::test::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn roles_and_route_guards_deny() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let new_account = json!({"code": "4000", "name": "Sales", "kind": "income"});
      let entry = json!({"memo": "m", "lines": [
        {"account_id": accounts[0].id, "amount": 1},
        {"account_id": accounts[1].id, "amount": -1},
      ]});

      // Anonymous: 401 everywhere but the public routes.
      for path in [
        "/accounts",
        "/me",
        "/entries",
        "/reports/trial-balance",
        "/audit/verify",
        "/_routes",
      ] {
        l.get(path, &As::Anonymous)
          .await
          .assert_status(StatusCode::UNAUTHORIZED);
      }
      for path in ["/openapi.json", "/docs", "/health"] {
        l.get(path, &As::Anonymous)
          .await
          .assert_status(StatusCode::OK);
      }

      // Viewer: reads, nothing else.
      l.get("/accounts", &team.viewer)
        .await
        .assert_status(StatusCode::OK);
      l.post("/accounts", new_account.clone(), &team.viewer)
        .await
        .assert_status(StatusCode::FORBIDDEN);
      l.post("/entries", entry.clone(), &team.viewer)
        .await
        .assert_status(StatusCode::FORBIDDEN);
      l.get("/export/postings.csv", &team.viewer)
        .await
        .assert_status(StatusCode::OK);

      // Accountant: posts and opens accounts, but does not audit or read the route table.
      l.post("/entries", entry.clone(), &team.accountant)
        .await
        .assert_status(StatusCode::CREATED);
      l.get("/audit/verify", &team.accountant)
        .await
        .assert_status(StatusCode::FORBIDDEN);
      l.get("/_routes", &team.accountant)
        .await
        .assert_status(StatusCode::FORBIDDEN);

      // Owner: all of it.
      l.post("/accounts", new_account, &team.owner)
        .await
        .assert_status(StatusCode::CREATED);
      l.get("/audit/verify", &team.owner)
        .await
        .assert_status(StatusCode::OK);
      l.get("/_routes", &team.owner)
        .await
        .assert_status(StatusCode::OK);

      // A denial is a Problem naming the policy, and every decision was recorded.
      let denied = l.post("/entries", entry, &team.viewer).await;
      assert_eq!(
        denied.header("content-type"),
        Some("application/problem+json")
      );
      assert!(denied.text().contains("ledger.post"), "{}", denied.text());
      let events = l.decisions.events();
      let deny = events
        .iter()
        .find(|e| e.outcome == Outcome::Deny && e.policy == "ledger.post")
        .expect("the viewer's denied posting was recorded");
      assert_eq!(deny.principal, "acme-viewer");
      assert!(
        events
          .iter()
          .any(|e| e.outcome == Outcome::Allow && e.policy == "audit.verify")
      );
    },
  )
  .await;
}

#[tokio::test]
async fn can_post_and_ownership_decide_on_the_account() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      l.user("acme", "acme-accountant-2", "accountant", None)
        .await;
      let colleague = l.login("acme-accountant-2", None).await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let (vault, cash) = (&accounts[0], accounts[1].id);

      // The account's opener locks it (ownership allows it) ...
      let locked = l
        .patch(
          &format!("/accounts/{}", vault.id),
          json!({"locked": true, "version": 1}),
          &team.accountant,
        )
        .await;
      locked.assert_status(StatusCode::OK);
      // ... a colleague may not change it back ...
      l.patch(
        &format!("/accounts/{}", vault.id),
        json!({"locked": false, "version": 2}),
        &colleague,
      )
      .await
      .assert_status(StatusCode::FORBIDDEN);
      l.delete(&format!("/accounts/{}", vault.id), &colleague)
        .await
        .assert_status(StatusCode::FORBIDDEN);

      // ... and `can_post`: a locked account takes postings from its opener and owners only.
      let entry = json!({"memo": "m", "lines": [
        {"account_id": vault.id, "amount": 10},
        {"account_id": cash, "amount": -10},
      ]});
      let refused = l.post("/entries", entry.clone(), &colleague).await;
      refused.assert_status(StatusCode::FORBIDDEN);
      assert!(
        refused.text().contains("account.post"),
        "{}",
        refused.text()
      );
      l.post("/entries", entry.clone(), &team.accountant)
        .await
        .assert_status(StatusCode::CREATED);
      l.post("/entries", entry, &team.owner)
        .await
        .assert_status(StatusCode::CREATED);

      // An organisation owner manages any account of the organisation.
      l.patch(
        &format!("/accounts/{}", vault.id),
        json!({"locked": false, "version": 2}),
        &team.owner,
      )
      .await
      .assert_status(StatusCode::OK);
      // The colleague's refused posting is on record; the reason is the last alternative the
      // policy tried (the account was locked, they did not open it, they are not an owner).
      let denied: Vec<_> = l
        .decisions
        .events()
        .into_iter()
        .filter(|e| e.policy == "account.post" && e.outcome == Outcome::Deny)
        .collect();
      assert_eq!(denied.len(), 1, "{denied:?}");
      assert_eq!(denied[0].principal, "acme-accountant-2");
      assert_eq!(denied[0].reason.as_deref(), Some("requires role owner"));
    },
  )
  .await;
}

#[tokio::test]
async fn every_route_names_its_policy() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let table: Value = l.get("/_routes", &team.owner).await.json().unwrap();
      let routes = table.as_array().expect("a JSON route table");
      assert!(routes.len() >= 20, "{table}");
      let mut public = Vec::new();
      for route in routes {
        let policies = route["policies"].as_array().unwrap();
        if route["access"] == "public" {
          assert!(policies.is_empty(), "{route}");
          let (method, path) = (route["method"].as_str(), route["path"].as_str());
          public.push(format!("{} {}", method.unwrap(), path.unwrap()));
          continue;
        }
        assert_eq!(route["access"], "guarded", "{route}");
        assert_eq!(
          policies.len(),
          1,
          "every other typed route has one guard: {route}"
        );
      }
      // Only signing up and signing in are open to anonymous callers.
      assert_eq!(public, ["POST /orgs", "POST /auth/login"], "{table}");
      let text = l
        .send(l.client.get("/_routes?format=text").header(
          "cookie",
          match &team.owner {
            As::Session(c) => c,
            _ => unreachable!(),
          },
        ))
        .await
        .text();
      assert!(
        text.contains("POST") && text.contains("/entries/batch"),
        "{text}"
      );
    },
  )
  .await;
}
