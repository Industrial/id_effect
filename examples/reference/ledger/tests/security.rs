//! `SecurityLayer::recommended` (sec.web): the security headers and a nonce CSP on every API
//! answer, and CSRF on every unsafe request a browser could be tricked into — a session cookie or
//! no credential at all — while a bearer-only API client needs no token.

mod common;

use common::{As, CSRF_HEADER, with_ledger};
use id_effect_kit::test::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn api_answers_carry_the_recommended_headers() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let res = l.get("/health", &As::Anonymous).await;
      res.assert_status(StatusCode::OK);
      assert_eq!(res.header("x-content-type-options"), Some("nosniff"));
      assert_eq!(res.header("x-frame-options"), Some("DENY"));
      assert!(res.header("strict-transport-security").is_some());
      assert!(res.header("referrer-policy").is_some());
      let csp = res.header("content-security-policy").expect("a CSP");
      assert!(csp.contains("'nonce-"), "{csp}");
      // The HTML pages (docs, console, previews) carry inline styles and scripts: no CSP, the
      // other headers still.
      let docs = l.get("/openapi.json", &As::Anonymous).await;
      assert_eq!(docs.header("x-frame-options"), Some("DENY"));
      assert!(docs.header("content-security-policy").is_none());
    },
  )
  .await;
}

#[tokio::test]
async fn unsafe_browser_requests_need_the_csrf_token() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      let accounts = l.accounts("acme", "acme-accountant", 2).await;
      let entry = json!({"memo": "csrf", "lines": [
        {"account_id": accounts[0].id, "amount": 1},
        {"account_id": accounts[1].id, "amount": -1},
      ]});
      let As::Session(session) = &team.accountant else {
        unreachable!()
      };
      // A session cookie without the token (a forged cross-site form): refused.
      let forged = l
        .send(
          l.client
            .post_json("/entries", &entry)
            .header("cookie", &format!("{session}; {}", l.csrf.cookie)),
        )
        .await;
      forged.assert_status(StatusCode::FORBIDDEN);
      // A token that is not the cookie's: refused.
      let wrong = l
        .send(
          l.client
            .post_json("/entries", &entry)
            .header("cookie", &format!("{session}; {}", l.csrf.cookie))
            .header(CSRF_HEADER, "not-the-token"),
        )
        .await;
      wrong.assert_status(StatusCode::FORBIDDEN);
      // An anonymous login without one: refused too (login CSRF).
      l.send(l.client.post_json(
        "/auth/login",
        &json!({"username": "acme-owner", "password": common::PASSWORD}),
      ))
      .await
      .assert_status(StatusCode::FORBIDDEN);
      // With the token: posted.
      l.post("/entries", entry.clone(), &team.accountant)
        .await
        .assert_status(StatusCode::CREATED);

      // A bearer-only API client (no cookie): nothing ambient to forge, no token needed.
      let token: Value = l
        .post("/auth/tokens", json!({"name": "ci"}), &team.accountant)
        .await
        .json()
        .unwrap();
      let bearer = token["token"].as_str().unwrap().to_owned();
      l.send(l.client.post_json("/entries", &entry).bearer(&bearer))
        .await
        .assert_status(StatusCode::CREATED);
      // The same token next to a cookie is a browser request again.
      l.send(
        l.client
          .post_json("/entries", &entry)
          .bearer(&bearer)
          .header("cookie", session),
      )
      .await
      .assert_status(StatusCode::FORBIDDEN);
    },
  )
  .await;
}
