//! Authentication: password login into a PostgreSQL session, API tokens (JWT bearer) and their
//! revocation, TOTP for owners, and the login rate limit.

mod common;

use common::{As, PASSWORD, totp_code, with_ledger};
use id_effect_kit::auth::TotpSecret;
use id_effect_kit::sql::params;
use id_effect_kit::test::http::StatusCode;
use serde_json::{Value, json};

#[tokio::test]
async fn password_login_starts_a_postgres_session() {
  with_ledger(
    |_, _| {},
    |l| async move {
      // Registering an organisation signs its owner in.
      let res = l
        .post(
          "/orgs",
          json!({"org": "acme", "username": "ada", "password": PASSWORD}),
          &As::Anonymous,
        )
        .await;
      res.assert_status(StatusCode::CREATED);
      let cookie = res.header("set-cookie").unwrap().to_owned();
      assert!(
        cookie.starts_with("id_session=") && cookie.contains("HttpOnly"),
        "{cookie}"
      );
      let ada = As::session(&res);

      let me: Value = l.get("/me", &ada).await.json().unwrap();
      assert_eq!(
        me,
        json!({"id": "ada", "org": "acme", "roles": ["owner"], "mfa": false})
      );
      // The session lives in PostgreSQL, keyed by a hash: the cookie value is not in the table.
      let As::Session(pair) = &ada else {
        unreachable!()
      };
      let id = pair.split_once('=').unwrap().1;
      let stored = l
        .scalar(
          "SELECT count(*)::int8 FROM id_effect_sessions WHERE user_id = 'ada' AND id_hash <> $1",
          params![id],
        )
        .await;
      assert_eq!(stored, 1);

      // A second login with the same password; then logout ends that session only.
      let again = l.login("ada", None).await;
      l.send(l.by(&again, l.client.post("/auth/logout")))
        .await
        .assert_status(StatusCode::NO_CONTENT);
      l.get("/me", &again)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
      l.get("/me", &ada).await.assert_status(StatusCode::OK);
    },
  )
  .await;
}

#[tokio::test]
async fn wrong_passwords_and_unknown_users_are_refused_alike() {
  with_ledger(
    |_, _| {},
    |l| async move {
      l.team("acme").await;
      for (username, password) in [("acme-viewer", "not the password"), ("nobody", PASSWORD)] {
        let res = l
          .post(
            "/auth/login",
            json!({"username": username, "password": password}),
            &As::Anonymous,
          )
          .await;
        res.assert_status(StatusCode::UNAUTHORIZED);
        assert_eq!(
          res.problem().unwrap().detail.as_deref(),
          Some("wrong username or password")
        );
      }
      l.get("/accounts", &As::Anonymous)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
      l.get("/accounts", &As::Session("id_session=forged".into()))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    },
  )
  .await;
}

#[tokio::test]
async fn api_tokens_are_bearer_jwts_that_can_be_revoked() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let team = l.team("acme").await;
      l.user("acme", "acme-accountant-2", "accountant", None)
        .await;
      let colleague = l.login("acme-accountant-2", None).await;

      let res = l
        .post("/auth/tokens", json!({"name": "ci"}), &team.accountant)
        .await;
      res.assert_status(StatusCode::CREATED);
      let created: Value = res.json().unwrap();
      let token = As::Bearer(created["token"].as_str().unwrap().to_owned());
      let id = created["id"].as_str().unwrap().to_owned();

      l.get("/accounts", &token)
        .await
        .assert_status(StatusCode::OK);
      let me: Value = l.get("/me", &token).await.json().unwrap();
      assert_eq!(
        (me["id"].clone(), me["mfa"].clone()),
        (json!("acme-accountant"), json!(false))
      );
      let mine: Value = l
        .get("/auth/tokens", &team.accountant)
        .await
        .json()
        .unwrap();
      assert_eq!(mine["tokens"][0]["id"], id.as_str());
      // A viewer may not mint tokens.
      l.post("/auth/tokens", json!({"name": "x"}), &team.viewer)
        .await
        .assert_status(StatusCode::FORBIDDEN);

      // Ownership: only the issuer revokes it.
      l.delete(&format!("/auth/tokens/{id}"), &colleague)
        .await
        .assert_status(StatusCode::FORBIDDEN);
      l.delete(&format!("/auth/tokens/{id}"), &team.accountant)
        .await
        .assert_status(StatusCode::NO_CONTENT);
      let revoked = l.get("/accounts", &token).await;
      revoked.assert_status(StatusCode::UNAUTHORIZED);
      assert!(revoked.text().contains("revoked"));

      let As::Bearer(raw) = &token else {
        unreachable!()
      };
      l.get("/accounts", &As::Bearer(format!("{raw}x")))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    },
  )
  .await;
}

#[tokio::test]
async fn owners_enrol_totp_and_need_it_to_manage_users() {
  with_ledger(
    |_, _| {},
    |l| async move {
      let ada = l
        .post(
          "/orgs",
          json!({"org": "acme", "username": "ada", "password": PASSWORD}),
          &As::Anonymous,
        )
        .await;
      let ada = As::session(&ada);
      let new_user = json!({"username": "bob", "password": PASSWORD, "role": "viewer"});
      // An owner without a second factor cannot manage users.
      l.post("/users", new_user.clone(), &ada)
        .await
        .assert_status(StatusCode::FORBIDDEN);

      // Enrolment: a secret, then a first code proving the authenticator has it.
      let enrolment: Value = l
        .post("/auth/totp/enrol", json!({}), &ada)
        .await
        .json()
        .unwrap();
      assert!(
        enrolment["uri"]
          .as_str()
          .unwrap()
          .starts_with("otpauth://totp/Ledger:ada?")
      );
      let secret = TotpSecret::from_base32(enrolment["secret"].as_str().unwrap()).unwrap();
      let code = totp_code(&secret, 0);
      l.post("/auth/totp/confirm", json!({"code": code}), &ada)
        .await
        .assert_status(StatusCode::NO_CONTENT);

      // From now on the password alone is not enough, and a code is good once.
      let without = l
        .post(
          "/auth/login",
          json!({"username": "ada", "password": PASSWORD}),
          &As::Anonymous,
        )
        .await;
      without.assert_status(StatusCode::UNAUTHORIZED);
      assert_eq!(
        without.problem().unwrap().detail.as_deref(),
        Some("a TOTP code is required")
      );
      let replayed = l
        .post(
          "/auth/login",
          json!({"username": "ada", "password": PASSWORD, "totp": code}),
          &As::Anonymous,
        )
        .await;
      replayed.assert_status(StatusCode::UNAUTHORIZED);

      // The next step's code signs in with MFA, and user management opens up.
      let owner = l.login("ada", Some(totp_code(&secret, 1))).await;
      let me: Value = l.get("/me", &owner).await.json().unwrap();
      assert_eq!(me["mfa"], true);
      l.post("/users", new_user, &owner)
        .await
        .assert_status(StatusCode::CREATED);
      let bob = l.login("bob", None).await;
      let me: Value = l.get("/me", &bob).await.json().unwrap();
      assert_eq!(me["roles"], json!(["viewer"]));
    },
  )
  .await;
}

#[tokio::test]
async fn logins_are_rate_limited() {
  with_ledger(
    |config, _| {
      config.auth.login_burst = 3.0;
      config.auth.login_per_second = 0.01;
    },
    |l| async move {
      // A burst of 3 attempts per client; the client is the address the reverse proxy reports.
      let body = json!({"username": "nobody", "password": "wrong password"});
      let attempt = |client: Option<&'static str>| {
        let request = l.browser(l.client.post_json("/auth/login", &body), None);
        l.send(match client {
          Some(client) => request.header("x-forwarded-for", client),
          None => request,
        })
      };
      for _ in 0..3 {
        attempt(Some("198.51.100.1"))
          .await
          .assert_status(StatusCode::UNAUTHORIZED);
      }
      let limited = attempt(Some("198.51.100.1")).await;
      limited.assert_status(StatusCode::TOO_MANY_REQUESTS);
      assert!(limited.header("retry-after").is_some());
      assert_eq!(
        limited.header("content-type"),
        Some("application/problem+json")
      );
      // Another client has its own bucket ...
      attempt(Some("203.0.113.7"))
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
      // ... and clients the service cannot tell apart share one: the limit always applies.
      for _ in 0..3 {
        attempt(None).await.assert_status(StatusCode::UNAUTHORIZED);
      }
      attempt(None)
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
    },
  )
  .await;
}

#[tokio::test]
async fn h5_ledger_login_is_throttled() {
  with_ledger(
    |config, _| config.auth.login_free_attempts = 2,
    |l| async move {
      l.team("acme").await;
      let attempt = |user: &'static str, password: &'static str, client: &'static str| {
        l.send(
          l.browser(
            l.client.post_json(
              "/auth/login",
              &json!({"username": user, "password": password}),
            ),
            None,
          )
          .header("x-forwarded-for", client),
        )
      };
      // Two free failures; the third attempt waits, even with the right password, and says
      // how long.
      for _ in 0..2 {
        attempt("acme-accountant", "wrong password!", "198.51.100.1")
          .await
          .assert_status(StatusCode::UNAUTHORIZED);
      }
      let throttled = attempt("acme-accountant", PASSWORD, "198.51.100.1").await;
      throttled.assert_status(StatusCode::TOO_MANY_REQUESTS);
      assert!(throttled.header("retry-after").is_some());
      // The account is locked from any address ...
      attempt("acme-accountant", PASSWORD, "203.0.113.7")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
      // ... and the address for any account; another client signs into another account.
      attempt("acme-viewer", PASSWORD, "198.51.100.1")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
      attempt("acme-viewer", PASSWORD, "203.0.113.7")
        .await
        .assert_status(StatusCode::OK);
    },
  )
  .await;
}

#[tokio::test]
async fn h2_ledger_ignores_spoofed_forwarded_for() {
  let limited = |config: &mut ledger::config::LedgerConfig| {
    config.auth.login_burst = 2.0;
    config.auth.login_per_second = 0.01;
  };
  let body = json!({"username": "nobody", "password": "wrong password"});
  // Behind the trusted proxy (the test profile's 127.0.0.1), which appends the address it saw:
  // a client prepending made-up hops still draws from its own bucket.
  let spoofed = body.clone();
  with_ledger(
    move |config, _| limited(config),
    move |l| async move {
      let attempt = |forwarded: &'static str| {
        l.send(
          l.browser(l.client.post_json("/auth/login", &spoofed), None)
            .header("x-forwarded-for", forwarded),
        )
      };
      for _ in 0..2 {
        attempt("198.51.100.1")
          .await
          .assert_status(StatusCode::UNAUTHORIZED);
      }
      for spoof in [
        "203.0.113.66, 198.51.100.1",
        "10.9.9.9, 192.0.2.1, 198.51.100.1",
      ] {
        attempt(spoof)
          .await
          .assert_status(StatusCode::TOO_MANY_REQUESTS);
      }
      // The client the proxy reports is a client of its own.
      attempt("192.0.2.1")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    },
  )
  .await;
  // With no trusted proxy (the default) the header is ignored: every value is the same peer.
  with_ledger(
    move |config, _| {
      limited(config);
      config.http.trusted_proxies.clear();
    },
    move |l| async move {
      let attempt = |forwarded: &'static str| {
        l.send(
          l.browser(l.client.post_json("/auth/login", &body), None)
            .header("x-forwarded-for", forwarded),
        )
      };
      for client in ["198.51.100.1", "198.51.100.2"] {
        attempt(client)
          .await
          .assert_status(StatusCode::UNAUTHORIZED);
      }
      attempt("198.51.100.3")
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS);
    },
  )
  .await;
}
