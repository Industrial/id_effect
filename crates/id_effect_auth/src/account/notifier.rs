//! [`AccountNotifier`]: how a verification or reset token reaches the account's owner, and the
//! recording [`MemoryAccountNotifier`] for tests.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::{Effect, UtcDateTime, fail, succeed, suspend};

use super::{Account, AccountError, AccountToken, TokenKind};

/// One mail to send: the account, what the token proves, the plaintext token for the link, and
/// how long it is accepted. `Debug` does not show the token.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountMail {
  /// What the token proves (which mail this is).
  pub kind: TokenKind,
  /// The recipient account.
  pub account: Account,
  /// The plaintext token, for the link. It exists only here and in the sent mail.
  pub token: AccountToken,
  /// When the token stops being accepted.
  pub expires_at: UtcDateTime,
  /// How long the token is accepted from issue ([`AccountsConfig`](super::AccountsConfig)'s
  /// TTL for its kind), for the mail's text.
  pub valid_for: Duration,
}

impl fmt::Debug for AccountMail {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("AccountMail")
      .field("kind", &self.kind)
      .field("account", &self.account)
      .field("token", &self.token)
      .field("expires_at", &self.expires_at)
      .finish_non_exhaustive()
  }
}

/// Delivers [`AccountMail`]s. `id_effect_mail` implements it (feature `auth`, `AccountMails`):
/// templates rendered and sent straight through its `Mailer`.
///
/// [`Accounts`](super::Accounts) calls it on a background fiber, after the request's response is
/// decided, so a slow mail transport neither delays nor (by its timing) reveals anything about
/// the request. An implementation retries what is worth retrying itself; the failure it returns
/// goes to [`Accounts::on_background_error`](super::Accounts::on_background_error).
///
/// The token is a credential for the account until it expires or is used. Do not persist the
/// mail (for example in a job queue table) where a database reader could take it; see the
/// [module docs](super) for the reasoning.
pub trait AccountNotifier: Send + Sync + 'static {
  /// Deliver `mail` to `mail.account.email`.
  fn notify(&self, mail: AccountMail) -> Effect<(), AccountError>;
}

#[derive(Default)]
struct MemoryNotifierState {
  mails: Vec<AccountMail>,
  fail: Option<String>,
}

/// [`AccountNotifier`] that records the mails, for tests. Clones share the record.
#[derive(Clone, Default)]
pub struct MemoryAccountNotifier {
  inner: Arc<Mutex<MemoryNotifierState>>,
}

impl fmt::Debug for MemoryAccountNotifier {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryAccountNotifier")
      .field("sent", &self.state().mails.len())
      .finish()
  }
}

impl MemoryAccountNotifier {
  /// A notifier that has sent nothing.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, MemoryNotifierState> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Every mail so far, oldest first.
  pub fn mails(&self) -> Vec<AccountMail> {
    self.state().mails.clone()
  }

  /// The latest mail of `kind`.
  pub fn last(&self, kind: TokenKind) -> Option<AccountMail> {
    self
      .state()
      .mails
      .iter()
      .rev()
      .find(|m| m.kind == kind)
      .cloned()
  }

  /// Make every later `notify` fail with [`AccountError::Notify`]`(message)`, or recover with
  /// `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    self.state().fail = message.map(str::to_owned);
  }
}

impl AccountNotifier for MemoryAccountNotifier {
  fn notify(&self, mail: AccountMail) -> Effect<(), AccountError> {
    let this = self.clone();
    suspend(move || {
      let mut st = this.state();
      match st.fail.clone() {
        Some(m) => fail(AccountError::Notify(m)),
        None => {
          st.mails.push(mail);
          succeed(())
        }
      }
    })
  }
}
