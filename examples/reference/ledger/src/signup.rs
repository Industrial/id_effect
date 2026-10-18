//! Self-service accounts: registration, email verification and password reset through
//! id_effect_auth's [`Accounts`] and [`AccountRoutes`], over the `users` table.
//!
//! | Route (`POST`) | What |
//! |---|---|
//! | `/auth/register` | `{"email", "password"}`: a user named by its address, unverified; a verification mail is sent |
//! | `/auth/verify` | `{"token"}`: the address is verified, the user may sign in |
//! | `/auth/verify/resend` | `{"email"}`: a new verification mail (same `202` for any address) |
//! | `/auth/password/forgot` | `{"email"}`: a reset mail (same `202` for any address) |
//! | `/auth/password/reset` | `{"token", "password"}`: a new password; every session of the user ends |
//!
//! A registered user signs in at `/auth/login` with its address as username once the address is
//! verified. It belongs to no organisation until it creates one (`POST /orgs`) or an owner adds
//! it, so every ledger route answers it `403` meanwhile. The tokens are single use and expire
//! (24 hours, 30 minutes); only their SHA-256 is stored (`id_effect_account_tokens`), and the
//! mails are sent directly, never through the job queue ([`crate::mail::account_mails`]). The
//! routes sit behind the per-client login limit.

use std::sync::Arc;

use id_effect::{Effect, fail};
use id_effect_kit::auth::PasswordHash;
use id_effect_kit::auth::account::{
  Account, AccountError, AccountRoutes, AccountStore, Accounts, PgAccountTokenStore,
};
use id_effect_kit::mail::AccountMails;
use id_effect_kit::sql::{FromRow, SqlClient, SqlClientService, SqlError, params};

use crate::app::App;
use crate::db;
use crate::policy::VIEWER;

/// [`AccountStore`] over the `users` table: the account id is the username, which a registered
/// user gets from its (lower-case) address.
#[derive(Clone)]
pub struct UserAccounts(Arc<dyn SqlClient>);

impl UserAccounts {
  /// Accounts in `db`.
  pub fn new(db: Arc<dyn SqlClient>) -> Self {
    Self(db)
  }

  fn one(
    &self,
    sql: &'static str,
    params: Vec<id_effect_kit::sql::SqlValue>,
  ) -> Effect<Option<Account>, AccountError> {
    db::query_all::<AccountRow>(&self.0, sql, params)
      .map(|rows| rows.into_iter().next().map(AccountRow::into_account))
      .map_err(storage)
  }
}

#[derive(Clone, Debug, FromRow)]
#[from_row(crate = id_effect_kit::sql)]
struct AccountRow {
  username: String,
  email: String,
  email_verified: bool,
}

impl AccountRow {
  fn into_account(self) -> Account {
    Account {
      id: self.username,
      email: self.email,
      email_verified: self.email_verified,
    }
  }
}

fn storage(e: SqlError) -> AccountError {
  AccountError::Storage(e.to_string())
}

impl AccountStore for UserAccounts {
  fn find_by_email(&self, email: &str) -> Effect<Option<Account>, AccountError> {
    self.one(
      "SELECT username, email, email_verified FROM users WHERE email = $1",
      params![email],
    )
  }

  fn create(&self, email: &str, password: &PasswordHash) -> Effect<Account, AccountError> {
    let sql = "INSERT INTO users (username, email, password_hash, role) VALUES ($1, $1, $2, $3) \
      RETURNING username, email, email_verified";
    db::query_one::<AccountRow>(&self.0, sql, params![email, password.as_str(), VIEWER])
      .map(AccountRow::into_account)
      .catch(|e| {
        if e.is_unique_violation() {
          fail(AccountError::EmailTaken)
        } else {
          fail(storage(e))
        }
      })
  }

  fn set_password(
    &self,
    id: &str,
    password: &PasswordHash,
  ) -> Effect<Option<Account>, AccountError> {
    self.one(
      "UPDATE users SET password_hash = $2 WHERE username = $1 \
       RETURNING username, email, email_verified",
      params![id, password.as_str()],
    )
  }

  fn mark_verified(&self, id: &str) -> Effect<Option<Account>, AccountError> {
    self.one(
      "UPDATE users SET email_verified = true WHERE username = $1 AND email IS NOT NULL \
       RETURNING username, email, email_verified",
      params![id],
    )
  }
}

/// The account flows of `app`: its users, the token table, `mails`, its password hasher and
/// sessions (a reset ends them). A mail that cannot be sent is logged; the user asks again.
pub fn accounts(app: &App, mails: AccountMails) -> Accounts {
  Accounts::new(
    UserAccounts::new(Arc::clone(&app.db)),
    PgAccountTokenStore::from_service(SqlClientService(Arc::clone(&app.db))),
    mails,
    app.hasher.clone(),
    app.sessions.clone(),
  )
  .on_background_error(|e| eprintln!("ledger: account mail: {e}"))
}

/// The five account routes under `/auth`.
pub fn routes(accounts: Accounts) -> axum::Router {
  AccountRoutes::new(accounts).router()
}

/// Whether a user may sign in: password users created by an owner always, registered users once
/// their address is verified.
pub fn may_sign_in(email: Option<&str>, verified: bool) -> Result<(), &'static str> {
  match (email, verified) {
    (Some(_), false) => Err("verify your email address first"),
    _ => Ok(()),
  }
}

#[cfg(test)]
mod tests {
  use super::may_sign_in;

  #[test]
  fn registered_users_sign_in_once_verified() {
    assert_eq!(may_sign_in(None, false), Ok(()));
    assert_eq!(may_sign_in(Some("a@b.c"), true), Ok(()));
    assert!(may_sign_in(Some("a@b.c"), false).is_err());
  }
}
