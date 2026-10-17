//! [`Account`], the [`AccountStore`] the application implements over its users table, and the
//! in-memory store.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use id_effect::{Effect, fail, succeed, suspend};

use super::AccountError;
use crate::apikey::random_hex;
use crate::password::PasswordHash;

/// An account as the flows see it: its id (the sessions' user id), its email address (lower
/// case, see [`normalize_email`](super::normalize_email)) and whether that address was
/// verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
  /// The account id; [`Sessions`](crate::Sessions) know the account's sessions by it.
  pub id: String,
  /// The email address.
  pub email: String,
  /// Whether the owner proved they read mail at `email`.
  pub email_verified: bool,
}

/// Where accounts live. Implement it over the application's users table; every method is an
/// [`Effect`], so it may do I/O. The password hash is written, never read: the flows here do not
/// log in (that is [`PasswordHasher::verify_or_dummy`](crate::PasswordHasher::verify_or_dummy)
/// in the application's login).
pub trait AccountStore: Send + Sync + 'static {
  /// The account with address `email` (already normalized), if any.
  fn find_by_email(&self, email: &str) -> Effect<Option<Account>, AccountError>;

  /// A new, unverified account with address `email` and password hash `password`.
  /// [`AccountError::EmailTaken`] when an account already has that address.
  fn create(&self, email: &str, password: &PasswordHash) -> Effect<Account, AccountError>;

  /// Replace the password hash of account `id`; the account, or `None` when there is none.
  fn set_password(
    &self,
    id: &str,
    password: &PasswordHash,
  ) -> Effect<Option<Account>, AccountError>;

  /// Mark the address of account `id` verified; the account, or `None` when there is none.
  fn mark_verified(&self, id: &str) -> Effect<Option<Account>, AccountError>;
}

#[derive(Default)]
struct MemoryAccountsState {
  accounts: HashMap<String, (Account, PasswordHash)>,
  fail: Option<String>,
}

/// In-memory [`AccountStore`] for tests and examples. Clones share the accounts; ids are 16
/// random hex digits.
#[derive(Clone, Default)]
pub struct MemoryAccountStore {
  inner: Arc<Mutex<MemoryAccountsState>>,
}

impl fmt::Debug for MemoryAccountStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryAccountStore")
      .field("len", &self.len())
      .finish()
  }
}

impl MemoryAccountStore {
  /// An empty store.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, MemoryAccountsState> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Number of accounts.
  pub fn len(&self) -> usize {
    self.state().accounts.len()
  }

  /// `true` when the store holds no account.
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// The account `id`.
  pub fn get(&self, id: &str) -> Option<Account> {
    self.state().accounts.get(id).map(|(a, _)| a.clone())
  }

  /// The stored password hash of account `id`.
  pub fn password_hash(&self, id: &str) -> Option<PasswordHash> {
    self.state().accounts.get(id).map(|(_, h)| h.clone())
  }

  /// Make every later operation fail with [`AccountError::Storage`]`(message)`, or recover with
  /// `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    self.state().fail = message.map(str::to_owned);
  }

  fn with<A: Send + 'static>(
    &self,
    f: impl FnOnce(&mut HashMap<String, (Account, PasswordHash)>) -> Result<A, AccountError>
    + Send
    + 'static,
  ) -> Effect<A, AccountError> {
    let this = self.clone();
    suspend(move || {
      let mut st = this.state();
      let result = match st.fail.clone() {
        Some(m) => Err(AccountError::Storage(m)),
        None => f(&mut st.accounts),
      };
      match result {
        Ok(a) => succeed(a),
        Err(e) => fail(e),
      }
    })
  }
}

impl AccountStore for MemoryAccountStore {
  fn find_by_email(&self, email: &str) -> Effect<Option<Account>, AccountError> {
    let email = email.to_owned();
    self.with(move |accounts| {
      Ok(
        accounts
          .values()
          .find(|(a, _)| a.email == email)
          .map(|(a, _)| a.clone()),
      )
    })
  }

  fn create(&self, email: &str, password: &PasswordHash) -> Effect<Account, AccountError> {
    let (email, password) = (email.to_owned(), password.clone());
    self.with(move |accounts| {
      if accounts.values().any(|(a, _)| a.email == email) {
        return Err(AccountError::EmailTaken);
      }
      let id = random_hex(8).map_err(|e| AccountError::Random(e.to_string()))?;
      let account = Account {
        id: id.clone(),
        email,
        email_verified: false,
      };
      accounts.insert(id, (account.clone(), password));
      Ok(account)
    })
  }

  fn set_password(
    &self,
    id: &str,
    password: &PasswordHash,
  ) -> Effect<Option<Account>, AccountError> {
    let (id, password) = (id.to_owned(), password.clone());
    self.with(move |accounts| {
      Ok(accounts.get_mut(&id).map(|(a, h)| {
        *h = password;
        a.clone()
      }))
    })
  }

  fn mark_verified(&self, id: &str) -> Effect<Option<Account>, AccountError> {
    let id = id.to_owned();
    self.with(move |accounts| {
      Ok(accounts.get_mut(&id).map(|(a, _)| {
        a.email_verified = true;
        a.clone()
      }))
    })
  }
}
