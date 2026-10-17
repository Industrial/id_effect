//! Self-service accounts (sec.authn #8, msg.email #3 #4): registration, email verification and
//! password reset through id_effect_auth's `AccountRoutes`, the mails rendered from the Ledger's
//! templates and delivered to Mailpit; `/_mail/previews` only where the profile mounts it.

mod common;

use common::{As, PASSWORD, link_token, mail_received, mailpit, with_ledger};
use id_effect_kit::test::http::StatusCode;
use serde_json::{Value, json};

const CAROL: &str = "carol@ledger.example";
const NEW_PASSWORD: &str = "a brand new passphrase";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn register_verify_sign_in_and_reset_the_password() {
  if !id_effect_kit::test::require_docker() {
    return;
  }
  let (mailpit, mailer) = mailpit().await;
  with_ledger(
    move |_, services| services.mailer = mailer,
    move |l| async move {
      let login = |password: &'static str| {
        l.post(
          "/auth/login",
          json!({"username": CAROL, "password": password}),
          &As::Anonymous,
        )
      };
      let registered = l
        .post(
          "/auth/register",
          json!({"email": "Carol@Ledger.example", "password": PASSWORD}),
          &As::Anonymous,
        )
        .await;
      registered.assert_status(StatusCode::CREATED);
      let account: Value = registered.json().unwrap();
      assert_eq!(
        (account["email"].clone(), account["email_verified"].clone()),
        (json!(CAROL), json!(false))
      );
      // An address registers once.
      l.post(
        "/auth/register",
        json!({"email": CAROL, "password": PASSWORD}),
        &As::Anonymous,
      )
      .await
      .assert_status(StatusCode::CONFLICT);

      // Not before the address is verified.
      let early = login(PASSWORD).await;
      early.assert_status(StatusCode::UNAUTHORIZED);
      assert_eq!(
        early.problem().unwrap().detail.as_deref(),
        Some("verify your email address first")
      );

      // The verification mail, from the Ledger's templates, carries a single-use link.
      let verify = mail_received(&mailpit, 1).await.remove(0);
      assert_eq!(verify["Subject"], "Confirm your email address");
      assert_eq!(verify["To"][0]["Address"], CAROL);
      assert_eq!(verify["From"]["Name"], "Ledger");
      let token = link_token(&verify);
      assert!(
        verify["Text"]
          .as_str()
          .unwrap()
          .contains(&format!("{}?token={token}", l.config.mail.verify_url))
      );
      let verified = l
        .post("/auth/verify", json!({"token": token}), &As::Anonymous)
        .await;
      verified.assert_status(StatusCode::OK);
      assert_eq!(verified.json::<Value>().unwrap()["email_verified"], true);
      l.post("/auth/verify", json!({"token": token}), &As::Anonymous)
        .await
        .assert_status(StatusCode::BAD_REQUEST);

      // Signed in, but in no organisation yet: the ledger refuses it (fail closed).
      let signed_in = login(PASSWORD).await;
      signed_in.assert_status(StatusCode::OK);
      let carol = As::session(&signed_in);
      l.get("/me", &carol)
        .await
        .assert_status(StatusCode::FORBIDDEN);
      l.get("/accounts", &carol)
        .await
        .assert_status(StatusCode::FORBIDDEN);

      // Forgot password: the same answer for any address; a mail only for a real one.
      let forgot = l
        .post(
          "/auth/password/forgot",
          json!({"email": CAROL}),
          &As::Anonymous,
        )
        .await;
      let unknown = l
        .post(
          "/auth/password/forgot",
          json!({"email": "nobody@ledger.example"}),
          &As::Anonymous,
        )
        .await;
      forgot.assert_status(StatusCode::ACCEPTED);
      unknown.assert_status(StatusCode::ACCEPTED);
      assert_eq!(forgot.text(), unknown.text());
      let reset = mail_received(&mailpit, 2).await.remove(0);
      assert_eq!(reset["Subject"], "Reset your password");
      l.post(
        "/auth/password/reset",
        json!({"token": link_token(&reset), "password": NEW_PASSWORD}),
        &As::Anonymous,
      )
      .await
      .assert_status(StatusCode::NO_CONTENT);

      // The reset ended every session; only the new password signs in.
      l.get("/me", &carol)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
      login(PASSWORD)
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
      login(NEW_PASSWORD).await.assert_status(StatusCode::OK);
      // The token table never holds a token, only its digest.
      let plain = l
        .scalar(
          "SELECT count(*)::int8 FROM id_effect_account_tokens WHERE secret_hash LIKE $1",
          vec![format!("%{}%", &token[17..]).into()],
        )
        .await;
      assert_eq!(plain, 0);
      assert_eq!(
        mail_received(&mailpit, 2).await.len(),
        2,
        "no mail for the unknown address"
      );
      let _ = id_effect_kit::axum::EffectExecutor::global()
        .run_effect(
          mailpit
            .container()
            .remove::<id_effect_kit::test::ContainerError, id_effect::HNil>(),
        )
        .await;
    },
  )
  .await;
}

#[tokio::test]
async fn mail_previews_are_served_in_dev_only() {
  // `test` (like `prod`) does not mount them.
  with_ledger(
    |_, _| {},
    |l| async move {
      assert!(!l.config.mail.previews);
      l.get("/_mail/previews", &As::Anonymous)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    },
  )
  .await;
  // `dev` does: every mail, rendered from the templates on disk.
  with_ledger(
    |config, _| config.mail.previews = true,
    |l| async move {
      let index = l.get("/_mail/previews", &As::Anonymous).await;
      index.assert_status(StatusCode::OK);
      let page = index.text();
      for name in [
        "statement",
        "statement-linked",
        "account-verify",
        "account-reset",
      ] {
        assert!(page.contains(name), "{name} in {page}");
      }
      let text = l
        .get("/_mail/previews/statement-linked/text", &As::Anonymous)
        .await;
      text.assert_status(StatusCode::OK);
      assert!(
        text.text().contains("the link works for 7 days"),
        "{}",
        text.text()
      );
      let html = l
        .get("/_mail/previews/statement/html", &As::Anonymous)
        .await;
      assert!(html.text().contains("acme-1000-2026-09.csv"));
    },
  )
  .await;
}

#[test]
fn the_dev_profile_mounts_the_previews() {
  let text = std::fs::read_to_string(format!("{}/dev.toml", common::CONFIG)).unwrap();
  assert!(text.contains("previews = true"));
  for profile in ["base", "test", "prod"] {
    let text = std::fs::read_to_string(format!("{}/{profile}.toml", common::CONFIG)).unwrap();
    assert!(!text.contains("previews = true"), "{profile}");
  }
}
