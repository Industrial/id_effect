//! `AccountMails` (feature `auth`): id_effect_auth's `AccountNotifier` over the `Mailer`.
//!
//! The property the account flows rely on: a mail carrying a token is handed to the `Mailer`
//! when `notify` runs and is stored nowhere on the way (no job payload), so the only persisted
//! form of the token is the digest in the token store.
#![cfg(feature = "auth")]

use std::time::{Duration, Instant};

use id_effect::{Cause, Effect, Exit, Schedule, UtcDateTime};
use id_effect_auth::account::{
  Account, AccountError, AccountMail, AccountNotifier, AccountToken, Accounts, MemoryAccountStore,
  MemoryAccountTokenStore, TokenKind,
};
use id_effect_auth::{MemorySessionStore, PasswordHasher, PasswordParams, SessionConfig, Sessions};
use id_effect_mail::auth::{RESET_MAIL, VERIFY_MAIL};
use id_effect_mail::{
  AccountMails, Address, MailError, Mailbox, MailerService, Templates, TestMailbox,
};

fn run<A: Send + 'static>(eff: Effect<A, AccountError>) -> Result<A, AccountError> {
  match eff.block_on() {
    Exit::Success(a) => Ok(a),
    Exit::Failure(Cause::Fail(e)) => Err(e),
    Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
  }
}

fn mails(mailbox: &TestMailbox) -> AccountMails {
  AccountMails::new(
    MailerService::new(mailbox.clone()),
    Mailbox::named("Ledger", Address::parse("accounts@ledger.example").unwrap()),
    "https://ledger.example/verify",
    "https://ledger.example/password/reset?lang=en",
  )
}

fn mail(kind: TokenKind, email: &str) -> AccountMail {
  let expires_at = UtcDateTime::unsafe_make(1_800_000_000_000);
  let (token, _) = AccountToken::generate(kind, "a1", expires_at).unwrap();
  AccountMail {
    kind,
    account: Account {
      id: "a1".into(),
      email: email.into(),
      email_verified: false,
    },
    token,
    expires_at,
    valid_for: Duration::from_secs(24 * 3600),
  }
}

#[test]
fn notify_hands_the_mail_straight_to_the_mailer() {
  let mailbox = TestMailbox::new();
  let mails = mails(&mailbox);
  for (kind, subject, link) in [
    (
      TokenKind::VerifyEmail,
      "Confirm your email address",
      "https://ledger.example/verify?token=",
    ),
    (
      TokenKind::ResetPassword,
      "Reset your password",
      "https://ledger.example/password/reset?lang=en&token=",
    ),
  ] {
    mailbox.clear();
    let m = mail(kind, "carol@ledger.example");
    let token = m.token.expose().to_owned();
    assert_eq!(mails.link(&m), format!("{link}{token}"));
    // Sent by the time `notify` completes: nothing in between holds it.
    run(mails.notify(m)).unwrap();
    mailbox.assert_count(1);
    let sent = mailbox.assert_sent_to("carol@ledger.example");
    assert_eq!(sent.subject(), subject);
    assert_eq!(sent.from().to_string(), "Ledger <accounts@ledger.example>");
    let text = sent.text().unwrap();
    assert!(text.contains(&format!("{link}{token}")), "{text}");
    assert!(text.contains("24 hours"), "{text}");
    let html = sent.html().unwrap();
    assert!(html.contains(&token) && html.contains("<a href="), "{html}");
  }
}

#[test]
fn transient_failures_are_retried_and_others_are_not() {
  let mailbox = TestMailbox::new();
  let temporary = || MailError::Temporary {
    code: 451,
    message: "try later".into(),
  };
  mailbox.fail_next(temporary());
  mailbox.fail_next(temporary());
  // No delay between attempts, to keep the test fast.
  let quick = mails(&mailbox).with_retry(Schedule::recurs(3).while_input(MailError::is_transient));
  run(quick.notify(mail(TokenKind::ResetPassword, "dan@ledger.example"))).unwrap();
  mailbox.assert_count(1);

  // Out of attempts: the last failure is reported.
  mailbox.clear();
  for _ in 0..4 {
    mailbox.fail_next(temporary());
  }
  let err = run(quick.notify(mail(TokenKind::ResetPassword, "dan@ledger.example"))).unwrap_err();
  assert!(
    matches!(&err, AccountError::Notify(m) if m.contains("try later")),
    "{err:?}"
  );
  mailbox.assert_none_sent();

  // A permanent refusal is not retried, even by the default schedule (so no backoff sleeps).
  mailbox.clear();
  mailbox.fail_next(MailError::Rejected {
    code: 550,
    message: "no such user".into(),
  });
  let started = Instant::now();
  let err =
    run(mails(&mailbox).notify(mail(TokenKind::VerifyEmail, "eve@ledger.example"))).unwrap_err();
  assert!(
    matches!(&err, AccountError::Notify(m) if m.contains("550")),
    "{err:?}"
  );
  assert!(started.elapsed() < Duration::from_millis(900));
  mailbox.assert_none_sent();
}

#[test]
fn a_bad_address_or_template_is_a_notify_error() {
  let mailbox = TestMailbox::new();
  let err =
    run(mails(&mailbox).notify(mail(TokenKind::VerifyEmail, "not an address"))).unwrap_err();
  assert!(matches!(err, AccountError::Notify(_)), "{err:?}");
  let missing = mails(&mailbox).with_templates(Templates::new());
  assert!(matches!(
    run(missing.notify(mail(TokenKind::VerifyEmail, "fay@ledger.example"))),
    Err(AccountError::Notify(m)) if m.contains(VERIFY_MAIL)
  ));
  mailbox.assert_none_sent();
}

#[test]
fn custom_templates_replace_the_defaults() {
  let mailbox = TestMailbox::new();
  let templates = Templates::new()
    .with_template(format!("{RESET_MAIL}.subject.txt"), "Reset for {{ email }}")
    .unwrap()
    .with_template(
      format!("{RESET_MAIL}.txt"),
      "{{ link }} until {{ expires_at }}",
    )
    .unwrap();
  let custom = mails(&mailbox).with_templates(templates);
  let m = mail(TokenKind::ResetPassword, "gus@ledger.example");
  let message = custom.message(&m).unwrap();
  assert_eq!(message.subject(), "Reset for gus@ledger.example");
  assert_eq!(
    message.text(),
    Some(format!("{} until 2027-01-15T08:00:00Z", custom.link(&m)).as_str())
  );
  assert!(format!("{custom:?}").contains("password/reset"));
}

/// The flows of id_effect_auth with `AccountMails`: the token travels in the mail only; the
/// token store keeps its digest.
#[test]
fn the_account_flows_mail_through_account_mails() {
  let mailbox = TestMailbox::new();
  let tokens = MemoryAccountTokenStore::new();
  let accounts = Accounts::new(
    MemoryAccountStore::new(),
    tokens.clone(),
    mails(&mailbox),
    PasswordHasher::new(PasswordParams::new(8, 1, 1).unwrap()),
    Sessions::new(MemorySessionStore::new(), SessionConfig::default()),
  );
  run(accounts.register("hal@ledger.example", "correct horse battery")).unwrap();
  let token_in = |subject: &str| -> String {
    for _ in 0..400 {
      if let Some(m) = mailbox.sent().into_iter().find(|m| m.subject() == subject) {
        let text = m.text().unwrap().to_owned();
        let at = text.find("token=").expect("a link") + "token=".len();
        return text[at..].split_whitespace().next().unwrap().to_owned();
      }
      std::thread::sleep(Duration::from_millis(5));
    }
    panic!("no mail `{subject}`")
  };
  let token = token_in("Confirm your email address");
  let (_, secret) = token.split_once('_').unwrap();
  assert!(
    tokens
      .records()
      .iter()
      .all(|r| !format!("{r:?} {}", r.digest_hex()).contains(secret)),
    "the store has the digest only"
  );
  assert!(run(accounts.verify_email(&token)).unwrap().email_verified);

  run(accounts.forgot_password("hal@ledger.example")).unwrap();
  let token = token_in("Reset your password");
  run(accounts.reset_password(&token, "staple tuna engine")).unwrap();
  assert!(tokens.is_empty());
}
