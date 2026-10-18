//! [`AccountMails`] (feature `auth`): `id_effect_auth`'s [`AccountNotifier`], rendering the
//! verification and password reset mails from [`Templates`] and sending them through the
//! [`Mailer`](crate::Mailer).
//!
//! # Sent directly, never through the jobs table
//!
//! An account mail carries a usable token: whoever reads it can verify the address or set the
//! account's password until the token expires or is used. `deliver_later` (feature `jobs`)
//! stores the whole message as the job's payload in the `id_effect_jobs` table, and a finished
//! job stays there until it is pruned (7 days by default), so any reader of that table
//! or its backups could take over the account. `AccountMails` therefore sends with
//! [`Mailer::send`](crate::Mailer::send) on the fiber `id_effect_auth`'s `Accounts` runs it on
//! (a background fiber, so the request's response does not wait for it), retrying a
//! [transient](MailError::is_transient) failure on [`retry`](AccountMails::with_retry)'s
//! schedule (by default 3 more attempts, 1 s, 2 s and 4 s apart). Nothing is written anywhere on
//! the way: the token exists in plaintext in memory and in the sent mail only, and the token
//! table keeps its SHA-256 digest. A mail that still fails is reported to `Accounts`'
//! `on_background_error` hook, and the user asks for a new link.
//!
//! # Templates
//!
//! Two mails, each a subject, a text and an HTML template: [`VERIFY_MAIL`]
//! (`account_verify`) and [`RESET_MAIL`] (`account_reset`). [`AccountMails::default_templates`]
//! has plain English ones; pass your own [`Templates`] (with the same mail names) to
//! [`with_templates`](AccountMails::with_templates). Their context:
//!
//! | Variable | Value |
//! |---|---|
//! | `email` | the account's address |
//! | `link` | the configured URL with `token=<token>` added to its query |
//! | `valid_for` | the token's lifetime in words (`30 minutes`, `24 hours`) |
//! | `expires_at` | the expiry, RFC 3339 in UTC |
//!
//! ```
//! use std::time::Duration;
//!
//! use id_effect::{Exit, UtcDateTime};
//! use id_effect_auth::account::{Account, AccountMail, AccountNotifier, AccountToken, TokenKind};
//! use id_effect_mail::{AccountMails, Address, MailerService, TestMailbox};
//!
//! let mailbox = TestMailbox::new();
//! let mails = AccountMails::new(
//!   MailerService::new(mailbox.clone()),
//!   Address::parse("accounts@ledger.example").unwrap(),
//!   "https://ledger.example/verify",
//!   "https://ledger.example/password/reset",
//! );
//! let expires_at = UtcDateTime::unsafe_make(1_800_000_000_000);
//! let (token, _record) = AccountToken::generate(TokenKind::ResetPassword, "a1", expires_at).unwrap();
//! let mail = AccountMail {
//!   kind: TokenKind::ResetPassword,
//!   account: Account { id: "a1".into(), email: "carol@ledger.example".into(), email_verified: true },
//!   token: token.clone(),
//!   expires_at,
//!   valid_for: Duration::from_secs(30 * 60),
//! };
//! assert!(matches!(mails.notify(mail).block_on(), Exit::Success(())));
//! let sent = mailbox.assert_sent_to("carol@ledger.example");
//! assert_eq!(sent.subject(), "Reset your password");
//! let link = format!("https://ledger.example/password/reset?token={}", token.expose());
//! assert!(sent.text().unwrap().contains(&link));
//! assert!(sent.text().unwrap().contains("30 minutes"));
//! ```

use std::fmt;
use std::time::Duration;

use id_effect::{Effect, Schedule, fail};
use id_effect_auth::account::{AccountError, AccountMail, AccountNotifier, TokenKind};

use crate::{MailError, Mailbox, MailerService, Message, Templates};

/// The verification mail's name in [`Templates`] (`account_verify.subject.txt`, `.txt`,
/// `.html`).
pub const VERIFY_MAIL: &str = "account_verify";
/// The password reset mail's name in [`Templates`] (`account_reset.subject.txt`, `.txt`,
/// `.html`).
pub const RESET_MAIL: &str = "account_reset";

const DEFAULT_TEMPLATES: [(&str, &str); 6] = [
  (
    "account_verify.subject.txt",
    include_str!("../templates/auth/account_verify.subject.txt"),
  ),
  (
    "account_verify.txt",
    include_str!("../templates/auth/account_verify.txt"),
  ),
  (
    "account_verify.html",
    include_str!("../templates/auth/account_verify.html"),
  ),
  (
    "account_reset.subject.txt",
    include_str!("../templates/auth/account_reset.subject.txt"),
  ),
  (
    "account_reset.txt",
    include_str!("../templates/auth/account_reset.txt"),
  ),
  (
    "account_reset.html",
    include_str!("../templates/auth/account_reset.html"),
  ),
];

/// [`AccountNotifier`] over the [`Mailer`](crate::Mailer): the verification and reset mails
/// rendered from [`Templates`] and sent directly, never enqueued (see the
/// [module docs](self)). Cheap to clone.
#[derive(Clone)]
pub struct AccountMails {
  mailer: MailerService,
  from: Mailbox,
  templates: Templates,
  verify_url: String,
  reset_url: String,
  retry: Schedule<(), MailError>,
}

impl fmt::Debug for AccountMails {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AccountMails")
      .field("from", &self.from)
      .field("verify_url", &self.verify_url)
      .field("reset_url", &self.reset_url)
      .finish_non_exhaustive()
  }
}

impl AccountMails {
  /// Mails from `from` through `mailer`, linking to `verify_url` and `reset_url` (the pages of
  /// your application that post the token to `id_effect_auth`'s `/auth/verify` and
  /// `/auth/password/reset`), with [`default_templates`](Self::default_templates) and the
  /// default retry.
  pub fn new(
    mailer: MailerService,
    from: impl Into<Mailbox>,
    verify_url: impl Into<String>,
    reset_url: impl Into<String>,
  ) -> Self {
    Self {
      mailer,
      from: from.into(),
      templates: Self::default_templates(),
      verify_url: verify_url.into(),
      reset_url: reset_url.into(),
      retry: Self::default_retry(),
    }
  }

  /// Plain English templates for [`VERIFY_MAIL`] and [`RESET_MAIL`].
  pub fn default_templates() -> Templates {
    DEFAULT_TEMPLATES
      .iter()
      .fold(Templates::new(), |templates, (name, source)| {
        templates
          .with_template(*name, *source)
          .unwrap_or_else(|e| panic!("built-in template {name}: {e}"))
      })
  }

  /// 3 more attempts after a [transient](MailError::is_transient) failure, 1 s, 2 s and 4 s
  /// apart; none after any other.
  pub fn default_retry() -> Schedule<(), MailError> {
    Schedule::exponential(Duration::from_secs(1))
      .intersect(Schedule::recurs(3))
      .while_input(MailError::is_transient)
      .map(|_| ())
  }

  /// Render with `templates` (which must have [`VERIFY_MAIL`] and [`RESET_MAIL`]).
  #[must_use]
  pub fn with_templates(mut self, templates: Templates) -> Self {
    self.templates = templates;
    self
  }

  /// Retry failed sends on `schedule` (stepped with each error; `Schedule::recurs(0)` for no
  /// retry). Keep it short: the token expires meanwhile.
  #[must_use]
  pub fn with_retry<Out: 'static>(mut self, schedule: Schedule<Out, MailError>) -> Self {
    self.retry = schedule.map(|_| ());
    self
  }

  /// The link `mail` carries: its kind's URL with `token=<token>` added to the query.
  pub fn link(&self, mail: &AccountMail) -> String {
    let base = match mail.kind {
      TokenKind::VerifyEmail => &self.verify_url,
      TokenKind::ResetPassword => &self.reset_url,
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}token={}", mail.token.expose())
  }

  /// `mail` rendered and addressed, as [`notify`](AccountNotifier::notify) sends it.
  ///
  /// # Errors
  ///
  /// [`MailError::InvalidMessage`] when the account's address is not a valid one or a template
  /// does not render.
  pub fn message(&self, mail: &AccountMail) -> Result<Message, MailError> {
    let name = match mail.kind {
      TokenKind::VerifyEmail => VERIFY_MAIL,
      TokenKind::ResetPassword => RESET_MAIL,
    };
    let to = crate::Address::parse(mail.account.email.clone())
      .map_err(|e| MailError::InvalidMessage(format!("account address: {e}")))?;
    let context = serde_json::json!({
      "email": mail.account.email,
      "link": self.link(mail),
      "valid_for": in_words(mail.valid_for),
      "expires_at": mail.expires_at.0.to_string(),
    });
    self
      .templates
      .message(name, &context)?
      .from(self.from.clone())
      .to(to)
      .build()
  }
}

/// `d` as `N hours` when it is whole hours, else `N minutes` (at least one).
fn in_words(d: Duration) -> String {
  let secs = d.as_secs();
  let plural = |n: u64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
  if secs >= 3600 && secs.is_multiple_of(3600) {
    plural(secs / 3600, "hour")
  } else {
    plural((secs / 60).max(1), "minute")
  }
}

fn notify_error(e: &MailError) -> AccountError {
  AccountError::Notify(e.to_string())
}

impl AccountNotifier for AccountMails {
  fn notify(&self, mail: AccountMail) -> Effect<(), AccountError> {
    let message = match self.message(&mail) {
      Ok(m) => m,
      Err(e) => return fail(notify_error(&e)),
    };
    let mailer = self.mailer.clone();
    Effect::retry(move || mailer.send(message.clone()), self.retry.clone())
      .map(|_| ())
      .map_err(|e| notify_error(&e))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn durations_read_as_words() {
    assert_eq!(in_words(Duration::from_secs(30 * 60)), "30 minutes");
    assert_eq!(in_words(Duration::from_secs(24 * 3600)), "24 hours");
    assert_eq!(in_words(Duration::from_secs(3600)), "1 hour");
    assert_eq!(in_words(Duration::from_secs(90 * 60)), "90 minutes");
    assert_eq!(in_words(Duration::from_secs(5)), "1 minute");
  }

  #[test]
  fn the_built_in_templates_have_both_mails() {
    let t = AccountMails::default_templates();
    let context = serde_json::json!({
      "email": "a@b.c", "link": "https://x/?token=t&a=1", "valid_for": "1 hour",
      "expires_at": "2027-01-15T08:00:00Z",
    });
    for name in [VERIFY_MAIL, RESET_MAIL] {
      let mail = t.render(name, &context).unwrap();
      assert!(mail.subject.is_some());
      assert!(mail.text.unwrap().contains("https://x/?token=t&a=1"));
      // Autoescaped (`&`, and `/` as `&#x2f;`), which an `href` decodes back.
      let html = mail.html.unwrap();
      assert!(html.contains("?token=t&amp;a=1"), "{html}");
    }
  }
}
