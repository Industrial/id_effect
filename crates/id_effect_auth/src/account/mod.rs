//! Account flows (feature `accounts`): registration, email verification, and forgot / reset
//! password with single-use expiring tokens.
//!
//! - [`Accounts`] runs the flows over an [`AccountStore`] (the application's users table), an
//!   [`AccountTokenStore`] ([`PgAccountTokenStore`] in production, [`MemoryAccountTokenStore`]
//!   in tests), an [`AccountNotifier`] that delivers the mails, the [`PasswordHasher`] and the
//!   application's [`Sessions`].
//! - [`AccountRoutes`] serves them as axum routes: `POST /auth/register`, `/auth/verify`,
//!   `/auth/verify/resend`, `/auth/password/forgot` and `/auth/password/reset`.
//!
//! # Tokens
//!
//! A token reads `<id>_<secret>` ([`AccountToken`]): 16 hex digits of public id, the store's
//! lookup key, and 256 bits of CSPRNG secret. Like an API key ([`crate::apikey`]) the store keeps
//! only the SHA-256 digest of the secret ([`TokenRecord`]), and a presented secret's digest is
//! compared with [`subtle`]'s constant-time equality. A token is accepted once: it is redeemed
//! only by the request whose [`AccountTokenStore::consume`] removed it. It expires after
//! [`AccountsConfig::verify_ttl`] (24 hours) or [`AccountsConfig::reset_ttl`] (30 minutes);
//! issuing a token removes the account's earlier ones of the same kind.
//!
//! # Forgot password does not reveal accounts
//!
//! [`Accounts::forgot_password`] (and [`Accounts::request_verification`]) answer `()` whether or
//! not an account has the address: the same status and body over HTTP. The request does the same
//! work either way, one account lookup and one background fiber, and the slow part (storing the
//! token and sending the mail) runs on that fiber after the response is decided, so neither the
//! mail transport's latency nor its failures show in the response. Registration does reveal that
//! an address is taken (`409`); put a rate limit in front of it.
//!
//! # The token is never persisted in plaintext
//!
//! A reset or verification mail holds a usable token. If it were stored on its way to the mail
//! transport, a reader of that store (a database dump, a backup, a support tool on the jobs
//! table) could take over the account. `id_effect_mail`'s `deliver_later` stores the whole message
//! in the `id_effect_jobs` table until the finished job is pruned (7 days by default), far longer
//! than the token lives. So the account mails do **not** go through a job queue:
//! `id_effect_mail`'s `AccountMails` renders and sends them directly through the `Mailer`, on the
//! background fiber, retrying transient failures a few times with backoff. The token exists in
//! plaintext only in memory and in the sent mail; the token table holds its digest. If a send
//! still fails, the failure goes to [`Accounts::on_background_error`] and the user asks again
//! (the request is idempotent and the old token is replaced). The trade-off is that a process
//! that dies while a mail is in flight loses that mail; with tokens this short-lived that is
//! better than a durable copy of a credential.
//!
//! ```
//! use std::time::Duration;
//!
//! use id_effect::{Cause, Exit};
//! use id_effect_auth::account::{
//!   AccountError, Accounts, MemoryAccountNotifier, MemoryAccountStore, MemoryAccountTokenStore,
//!   TokenKind,
//! };
//! use id_effect_auth::{MemorySessionStore, PasswordHasher, PasswordParams, SessionConfig, Sessions};
//!
//! let notifier = MemoryAccountNotifier::new();
//! let accounts = Accounts::new(
//!   MemoryAccountStore::new(),
//!   MemoryAccountTokenStore::new(),
//!   notifier.clone(),
//!   PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap()), // low cost for docs
//!   Sessions::new(MemorySessionStore::new(), SessionConfig::default()),
//! );
//!
//! let Exit::Success(account) =
//!   accounts.register("Carol@Ledger.example", "correct horse battery").block_on()
//! else {
//!   panic!()
//! };
//! assert_eq!(account.email, "carol@ledger.example");
//! # // The verification mail goes out on a background fiber.
//! # let wait = || {
//! #   for _ in 0..200 {
//! #     if let Some(m) = notifier.last(TokenKind::VerifyEmail) { return m; }
//! #     std::thread::sleep(Duration::from_millis(5));
//! #   }
//! #   panic!("no mail")
//! # };
//! let mail = wait();
//! let Exit::Success(verified) = accounts.verify_email(mail.token.expose()).block_on() else {
//!   panic!()
//! };
//! assert!(verified.email_verified);
//! // A token is accepted once.
//! assert!(matches!(
//!   accounts.verify_email(mail.token.expose()).block_on(),
//!   Exit::Failure(Cause::Fail(AccountError::InvalidToken))
//! ));
//! ```

mod flows;
mod notifier;
mod pg;
mod routes;
mod store;
mod token;

use std::fmt;
use std::time::Duration;

pub use flows::Accounts;
pub use notifier::{AccountMail, AccountNotifier, MemoryAccountNotifier};
pub use pg::PgAccountTokenStore;
pub use routes::{AccountRoutes, DEFAULT_BASE_PATH};
pub use store::{Account, AccountStore, MemoryAccountStore};
pub use token::{AccountToken, AccountTokenStore, MemoryAccountTokenStore, TokenKind, TokenRecord};

use crate::password::PasswordError;
use crate::session::SessionError;
#[cfg(doc)]
use crate::{PasswordHasher, Sessions};

/// Token lifetimes and the password policy of [`Accounts`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountsConfig {
  /// How long a verification token is accepted (default 24 hours).
  pub verify_ttl: Duration,
  /// How long a password reset token is accepted (default 30 minutes; OWASP's Forgot Password
  /// Cheat Sheet asks for a short lifetime).
  pub reset_ttl: Duration,
  /// Fewest characters a new password may have (default 12).
  pub min_password_len: usize,
  /// Most characters a new password may have (default 256), so a request cannot make argon2
  /// hash megabytes.
  pub max_password_len: usize,
}

impl Default for AccountsConfig {
  fn default() -> Self {
    Self {
      verify_ttl: Duration::from_secs(24 * 60 * 60),
      reset_ttl: Duration::from_secs(30 * 60),
      min_password_len: 12,
      max_password_len: 256,
    }
  }
}

impl AccountsConfig {
  /// The lifetime of a token of `kind`.
  pub fn ttl(&self, kind: TokenKind) -> Duration {
    match kind {
      TokenKind::VerifyEmail => self.verify_ttl,
      TokenKind::ResetPassword => self.reset_ttl,
    }
  }

  /// [`AccountError::WeakPassword`] when `password` is shorter or longer than allowed.
  ///
  /// # Errors
  ///
  /// As above.
  pub fn check_password(&self, password: &str) -> Result<(), AccountError> {
    let len = password.chars().count();
    if len < self.min_password_len {
      return Err(AccountError::WeakPassword(format!(
        "a password needs at least {} characters",
        self.min_password_len
      )));
    }
    if len > self.max_password_len {
      return Err(AccountError::WeakPassword(format!(
        "a password may have at most {} characters",
        self.max_password_len
      )));
    }
    Ok(())
  }
}

/// `raw` trimmed and lower-cased, if it has the shape of a mailbox address: `local@domain`, no
/// whitespace or control characters, a local part of at most 64 bytes, a domain of non-empty
/// dot-separated labels, at most 254 bytes in all. Accounts compare addresses after this, so
/// `Carol@Example.com` and `carol@example.com` are one account.
pub fn normalize_email(raw: &str) -> Option<String> {
  let email = raw.trim();
  let (local, domain) = email.split_once('@')?;
  let ok = !local.is_empty()
    && local.len() <= 64
    && email.len() <= 254
    && !domain.contains('@')
    && !domain.split('.').any(str::is_empty)
    && !email.chars().any(|c| c.is_whitespace() || c.is_control());
  ok.then(|| email.to_lowercase())
}

/// Why an account flow did not complete.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AccountError {
  /// The address is not shaped like one ([`normalize_email`]).
  InvalidEmail,
  /// The new password breaks [`AccountsConfig`]'s policy; the message says how.
  WeakPassword(String),
  /// An account already has the address (registration).
  EmailTaken,
  /// The token is malformed, unknown, already used, of the other kind, or its secret is wrong.
  InvalidToken,
  /// The token was right but has expired (it is removed).
  ExpiredToken,
  /// The account or token store failed.
  Storage(String),
  /// Hashing the new password failed.
  Password(PasswordError),
  /// Revoking the account's sessions failed.
  Session(SessionError),
  /// The notifier could not deliver a mail.
  Notify(String),
  /// The operating system's random source failed while minting a token or id.
  Random(String),
}

impl AccountError {
  /// 422 for an invalid address or password, 409 for a taken address, 400 for a rejected token,
  /// 503 for a store, session or mail outage, 500 otherwise.
  pub fn http_status(&self) -> u16 {
    match self {
      Self::InvalidEmail | Self::WeakPassword(_) => 422,
      Self::EmailTaken => 409,
      Self::InvalidToken | Self::ExpiredToken => 400,
      Self::Storage(_) | Self::Session(_) | Self::Notify(_) => 503,
      Self::Password(_) | Self::Random(_) => 500,
    }
  }
}

impl fmt::Display for AccountError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::InvalidEmail => f.write_str("not a valid email address"),
      Self::WeakPassword(m) => f.write_str(m),
      Self::EmailTaken => f.write_str("an account with this email address exists"),
      Self::InvalidToken => f.write_str("the link is invalid or was already used"),
      Self::ExpiredToken => f.write_str("the link has expired; ask for a new one"),
      Self::Storage(m) => write!(f, "account storage: {m}"),
      Self::Password(e) => write!(f, "{e}"),
      Self::Session(e) => write!(f, "{e}"),
      Self::Notify(m) => write!(f, "account mail: {m}"),
      Self::Random(m) => write!(f, "random source: {m}"),
    }
  }
}

impl std::error::Error for AccountError {}

impl From<PasswordError> for AccountError {
  fn from(e: PasswordError) -> Self {
    Self::Password(e)
  }
}

impl From<SessionError> for AccountError {
  fn from(e: SessionError) -> Self {
    Self::Session(e)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn defaults_are_short_lived_tokens_and_a_12_character_minimum() {
    let c = AccountsConfig::default();
    assert_eq!(c.ttl(TokenKind::ResetPassword), Duration::from_secs(1800));
    assert_eq!(c.ttl(TokenKind::VerifyEmail), Duration::from_secs(86_400));
    assert!(c.check_password("eleven char").is_err());
    assert!(c.check_password("twelve chars").is_ok());
    assert!(c.check_password(&"x".repeat(257)).is_err());
  }

  #[test]
  fn addresses_are_trimmed_lowercased_and_checked() {
    assert_eq!(
      normalize_email("  Carol@Ledger.Example "),
      Some("carol@ledger.example".into())
    );
    for bad in [
      "",
      "carol",
      "@ledger.example",
      "carol@",
      "carol@ledger..example",
      "carol@a@b",
      "ca rol@ledger.example",
      &format!("{}@x.y", "a".repeat(65)),
    ] {
      assert_eq!(normalize_email(bad), None, "{bad}");
    }
  }

  #[test]
  fn statuses_and_messages() {
    let cases = [
      (AccountError::InvalidEmail, 422),
      (AccountError::WeakPassword("short".into()), 422),
      (AccountError::EmailTaken, 409),
      (AccountError::InvalidToken, 400),
      (AccountError::ExpiredToken, 400),
      (AccountError::Storage("db".into()), 503),
      (
        AccountError::Session(SessionError::Storage("db".into())),
        503,
      ),
      (AccountError::Notify("smtp".into()), 503),
      (AccountError::Password(PasswordError::Blocking), 500),
      (AccountError::Random("rng".into()), 500),
    ];
    for (error, status) in cases {
      assert_eq!(error.http_status(), status, "{error:?}");
      assert!(!error.to_string().is_empty());
    }
    assert_eq!(
      AccountError::from(PasswordError::Blocking),
      AccountError::Password(PasswordError::Blocking)
    );
  }
}
