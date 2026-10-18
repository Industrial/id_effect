//! [`Accounts`]: registration, email verification and forgot / reset password.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use id_effect::{Effect, Never, UtcDateTime, WallClock, fail, succeed};

use super::{
  Account, AccountError, AccountMail, AccountNotifier, AccountStore, AccountToken,
  AccountTokenStore, AccountsConfig, TokenKind, TokenRecord, normalize_email,
};
use crate::password::{Password, PasswordHasher};
use crate::session::Sessions;

/// `t + d`, saturating at the end of time.
fn plus(t: UtcDateTime, d: Duration) -> UtcDateTime {
  UtcDateTime(t.0.checked_add(d).unwrap_or(jiff::Timestamp::MAX))
}

type ErrorHook = Arc<dyn Fn(&AccountError) + Send + Sync>;

/// The account flows over the application's stores, notifier, password hasher and sessions.
/// Cheap to clone. See the [module docs](super) for the security properties.
///
/// | Method | Does | Fails with |
/// |---|---|---|
/// | [`register`](Self::register) | a new unverified account; a verification mail in the background | `InvalidEmail`, `WeakPassword`, `EmailTaken` |
/// | [`verify_email`](Self::verify_email) | redeems a verification token; the account is verified | `InvalidToken`, `ExpiredToken` |
/// | [`request_verification`](Self::request_verification) | a fresh verification mail for an unverified account, `()` either way | `InvalidEmail` |
/// | [`forgot_password`](Self::forgot_password) | a reset mail for the account, `()` either way | `InvalidEmail` |
/// | [`reset_password`](Self::reset_password) | redeems a reset token: the new password is hashed and stored, every session of the account is revoked | `WeakPassword`, `InvalidToken`, `ExpiredToken` |
///
/// Any of them also fails with `Storage`, `Password` or `Session` when a backend does.
#[derive(Clone)]
pub struct Accounts {
  accounts: Arc<dyn AccountStore>,
  tokens: Arc<dyn AccountTokenStore>,
  notifier: Arc<dyn AccountNotifier>,
  hasher: PasswordHasher,
  sessions: Sessions,
  config: Arc<AccountsConfig>,
  clock: WallClock,
  on_error: ErrorHook,
}

impl fmt::Debug for Accounts {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Accounts")
      .field("config", &self.config)
      .field("hasher", &self.hasher)
      .field("clock", &self.clock)
      .finish_non_exhaustive()
  }
}

impl Accounts {
  /// The flows over `accounts` and `tokens`, mailing through `notifier`, hashing with `hasher`
  /// and revoking `sessions` on a password reset; [`AccountsConfig::default`], the system clock,
  /// and background failures discarded (see [`on_background_error`](Self::on_background_error)).
  pub fn new(
    accounts: impl AccountStore,
    tokens: impl AccountTokenStore,
    notifier: impl AccountNotifier,
    hasher: PasswordHasher,
    sessions: Sessions,
  ) -> Self {
    Self {
      accounts: Arc::new(accounts),
      tokens: Arc::new(tokens),
      notifier: Arc::new(notifier),
      hasher,
      sessions,
      config: Arc::new(AccountsConfig::default()),
      clock: WallClock::system(),
      on_error: Arc::new(|_| {}),
    }
  }

  /// The same flows with token lifetimes and password policy `config`.
  #[must_use]
  pub fn with_config(mut self, config: AccountsConfig) -> Self {
    self.config = Arc::new(config);
    self
  }

  /// The same flows read time from `clock` (an anchored clock lets tests expire tokens).
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// Call `hook` with every failure of the background work (storing a token, sending its mail).
  /// Those failures cannot reach the request, whose response does not wait for the work; log
  /// them here. The default discards them.
  #[must_use]
  pub fn on_background_error(
    mut self,
    hook: impl Fn(&AccountError) + Send + Sync + 'static,
  ) -> Self {
    self.on_error = Arc::new(hook);
    self
  }

  /// Token lifetimes and password policy.
  #[inline]
  pub fn config(&self) -> &AccountsConfig {
    &self.config
  }

  fn now(&self) -> Effect<UtcDateTime, AccountError> {
    self.clock.now()
  }

  /// A new account with address `email` (normalized by
  /// [`normalize_email`](super::normalize_email)) and `password` (checked against the policy,
  /// then hashed). A verification mail goes out on a background fiber.
  pub fn register(
    &self,
    email: &str,
    password: impl Into<Password>,
  ) -> Effect<Account, AccountError> {
    let Some(email) = normalize_email(email) else {
      return fail(AccountError::InvalidEmail);
    };
    let password = password.into();
    if let Err(e) = self.config.check_password(password.expose()) {
      return fail(e);
    }
    let this = self.clone();
    self
      .hasher
      .hash(password)
      .map_err(AccountError::from)
      .flat_map(move |hash| {
        this
          .accounts
          .create(&email, &hash)
          .flat_map(move |account| {
            let work = this.issue(account.clone(), TokenKind::VerifyEmail);
            this.background(work).map(move |()| account)
          })
      })
  }

  /// Redeem a verification token: the account's address is verified.
  pub fn verify_email(&self, token: &str) -> Effect<Account, AccountError> {
    let this = self.clone();
    self
      .redeem(token, TokenKind::VerifyEmail)
      .flat_map(move |record| {
        let tokens = Arc::clone(&this.tokens);
        this
          .accounts
          .mark_verified(record.account_id())
          .flat_map(move |found| match found {
            Some(account) => tokens
              .remove_account(&account.id, TokenKind::VerifyEmail)
              .map(move |_| account),
            None => fail(AccountError::InvalidToken),
          })
      })
  }

  /// A fresh verification mail for the unverified account with address `email`. Answers `()`
  /// whether or not there is one (see the [module docs](super)).
  pub fn request_verification(&self, email: &str) -> Effect<(), AccountError> {
    self.uniform(email, TokenKind::VerifyEmail, |a| !a.email_verified)
  }

  /// A password reset mail for the account with address `email`. Answers `()` whether or not
  /// there is one: the same work on the request's fiber (one lookup, one background fiber), and
  /// the token and mail on the background fiber (see the [module docs](super)).
  pub fn forgot_password(&self, email: &str) -> Effect<(), AccountError> {
    self.uniform(email, TokenKind::ResetPassword, |_| true)
  }

  fn uniform(
    &self,
    email: &str,
    kind: TokenKind,
    wanted: fn(&Account) -> bool,
  ) -> Effect<(), AccountError> {
    let Some(email) = normalize_email(email) else {
      return fail(AccountError::InvalidEmail);
    };
    let this = self.clone();
    self.accounts.find_by_email(&email).flat_map(move |found| {
      // Either way one background fiber, so the request's work does not depend on the answer.
      let work = match found.filter(wanted) {
        Some(account) => this.issue(account, kind),
        None => succeed(()),
      };
      this.background(work)
    })
  }

  /// Redeem a reset token with `password` as the account's new password: it is checked against
  /// the policy and hashed with the [`PasswordHasher`] before the token is consumed, then stored,
  /// and every session of the account is revoked (whoever had the old password is signed out).
  /// The account's other reset tokens are removed.
  pub fn reset_password(
    &self,
    token: &str,
    password: impl Into<Password>,
  ) -> Effect<Account, AccountError> {
    let password = password.into();
    if let Err(e) = self.config.check_password(password.expose()) {
      return fail(e);
    }
    let this = self.clone();
    self
      .check(token, TokenKind::ResetPassword)
      .flat_map(move |record| {
        this
          .hasher
          .hash(password)
          .map_err(AccountError::from)
          .flat_map(move |hash| {
            this.consume(record).flat_map(move |record| {
              let this2 = this.clone();
              this
                .accounts
                .set_password(record.account_id(), &hash)
                .flat_map(move |found| match found {
                  Some(account) => this2.after_reset(account),
                  None => fail(AccountError::InvalidToken),
                })
            })
          })
      })
  }

  fn after_reset(&self, account: Account) -> Effect<Account, AccountError> {
    let tokens = Arc::clone(&self.tokens);
    self
      .sessions
      .revoke_all(&account.id)
      .map_err(AccountError::from)
      .flat_map(move |_| {
        tokens
          .remove_account(&account.id, TokenKind::ResetPassword)
          .map(move |_| account)
      })
  }

  /// Remove every expired token from the store; how many. Schedule it (for example with
  /// `Effect::repeat` over `Schedule::spaced`).
  pub fn purge_expired_tokens(&self) -> Effect<u64, AccountError> {
    let tokens = Arc::clone(&self.tokens);
    self.now().flat_map(move |now| tokens.purge_expired(now))
  }

  /// A token of `kind` for `account` (replacing its earlier ones), stored as its digest, and
  /// its mail.
  fn issue(&self, account: Account, kind: TokenKind) -> Effect<(), AccountError> {
    let this = self.clone();
    let valid_for = self.config.ttl(kind);
    self.now().flat_map(move |now| {
      let expires_at = plus(now, valid_for);
      let (token, record) = match AccountToken::generate(kind, account.id.clone(), expires_at) {
        Ok(issued) => issued,
        Err(e) => return fail(e),
      };
      let (tokens, notifier) = (Arc::clone(&this.tokens), Arc::clone(&this.notifier));
      this
        .tokens
        .remove_account(&account.id, kind)
        .flat_map(move |_| tokens.insert(record))
        .flat_map(move |()| {
          notifier.notify(AccountMail {
            kind,
            account,
            token,
            expires_at,
            valid_for,
          })
        })
    })
  }

  /// Runs `work` on a daemon fiber (it outlives the request) with its failure going to the
  /// hook; succeeds as soon as the fiber is started.
  fn background(&self, work: Effect<(), AccountError>) -> Effect<(), AccountError> {
    let hook = Arc::clone(&self.on_error);
    work
      .catch(move |e| -> Effect<(), Never> {
        hook(&e);
        succeed(())
      })
      .fork_daemon()
      .widen()
      .map(|_| ())
  }

  /// The live record of the presented token of `kind`, not yet consumed. An expired one is
  /// removed and fails with [`AccountError::ExpiredToken`].
  fn check(&self, presented: &str, kind: TokenKind) -> Effect<TokenRecord, AccountError> {
    let Some((id, secret)) = AccountToken::split(presented) else {
      return fail(AccountError::InvalidToken);
    };
    let secret = secret.to_owned();
    let this = self.clone();
    self.tokens.find(id).flat_map(move |found| {
      let record = match found {
        Some(r) if r.matches(&secret) && r.kind() == kind => r,
        _ => return fail(AccountError::InvalidToken),
      };
      this.now().flat_map(move |now| {
        if now >= record.expires_at() {
          return this
            .tokens
            .consume(record.id())
            .flat_map(|_| fail(AccountError::ExpiredToken));
        }
        succeed(record)
      })
    })
  }

  /// `record`, once this request removed it from the store (single use).
  fn consume(&self, record: TokenRecord) -> Effect<TokenRecord, AccountError> {
    self.tokens.consume(record.id()).flat_map(move |removed| {
      if removed {
        succeed(record)
      } else {
        fail(AccountError::InvalidToken)
      }
    })
  }

  /// [`check`](Self::check) and [`consume`](Self::consume).
  fn redeem(&self, presented: &str, kind: TokenKind) -> Effect<TokenRecord, AccountError> {
    let this = self.clone();
    self
      .check(presented, kind)
      .flat_map(move |record| this.consume(record))
  }
}
