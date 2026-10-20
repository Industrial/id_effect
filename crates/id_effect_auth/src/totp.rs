//! TOTP second factor (RFC 6238, over totp-rs): enrolment, verification on the executor's clock
//! with replay protection, argon2-hashed recovery codes, and an MFA step-up marker on the
//! session.
//!
//! - [`Totp`] enrols a user ([`Totp::enrol`]: a random 160-bit [`TotpSecret`] and the
//!   `otpauth://totp/…` URI an authenticator app scans) and verifies codes
//!   ([`Totp::verify`]). The time comes from a [`WallClock`], so verification runs in virtual
//!   time under a `TestExecutor` or an anchored clock. A code is accepted for the current step
//!   and ±[`TotpConfig::skew`] steps (one by default, RFC 6238 §5.2), and only once: the
//!   [`TotpStepStore`] remembers the last step each user consumed, and a code for that step or
//!   an earlier one is [`TotpVerification::Replayed`]. With several instances, use
//!   `PgTotpStepStore` (features `pg` + `totp`): a code consumed on one instance is a replay on
//!   every other. [`MemoryTotpStepStore`] is per process, and creating one records
//!   [`MEMORY_TOTP_STEP_STORE_POSTURE`] in the posture registry
//!   ([`id_effect::observability::posture`]) as memory-only security state, which the prod
//!   posture check reports.
//! - [`RecoveryCodes`] generates single-use recovery codes, shown once and stored as argon2id
//!   hashes through [`PasswordHasher`]; [`RecoveryCodes::redeem`] checks a code against every
//!   stored hash (no early exit) and hands back the remaining ones to store.
//! - After a second factor, [`Totp::step_up`] marks the session with the time
//!   ([`MFA_VERIFIED_AT_ATTR`]) and rotates its id; [`with_mfa_claims`] turns the mark into the
//!   principal's `amr` claim (RFC 8176 `otp`, `mfa`), which [`require_mfa`] (an
//!   `id_effect_authz` claim policy) requires.
//!
//! A 6-digit code over three steps is guessed with probability 3·10⁻⁶ per try, so failures are
//! counted per user: after [`DEFAULT_MAX_FAILURES`] (5) wrong codes, each less than
//! [`DEFAULT_LOCKOUT`] (15 minutes) after the one before, [`Totp::verify`] fails with
//! [`TotpError::Locked`] until that long has passed since the last one; a valid code clears the
//! count. The counts live in a [`ThrottleStore`] (in memory by default, see
//! [`Totp::with_failure_store`]).
//!
//! ```
//! use std::time::Instant;
//!
//! use id_effect::{Exit, UtcDateTime, WallClock};
//! use id_effect_auth::totp::{MemoryTotpStepStore, Totp, TotpConfig, TotpVerification};
//!
//! let clock = WallClock::anchored(Instant::now(), UtcDateTime::unsafe_make(1_800_000_000_000));
//! let totp = Totp::new(TotpConfig::new("Acme"), MemoryTotpStepStore::new()).with_clock(clock);
//!
//! let enrolment = totp.enrol("alice@example.com").unwrap();
//! assert!(enrolment.uri().starts_with("otpauth://totp/Acme:alice%40example.com?secret="));
//!
//! // The code the user's app shows now (computed here from the same secret).
//! let code = totp.code_at(enrolment.secret(), UtcDateTime::unsafe_make(1_800_000_000_000)).unwrap();
//! let check = totp.verify("alice", enrolment.secret(), &code);
//! assert!(matches!(check.block_on(), Exit::Success(TotpVerification::Valid)));
//! let again = totp.verify("alice", enrolment.secret(), &code);
//! assert!(matches!(again.block_on(), Exit::Success(TotpVerification::Replayed)));
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use id_effect::{Effect, UtcDateTime, WallClock, fail, succeed, suspend};
use id_effect_authz::{ClaimContains, ClaimPolicy, claim_contains};
use serde_json::Value;
use totp_rs::{Builder, Secret};

pub use totp_rs::Algorithm;

use crate::password::{PasswordError, PasswordHash, PasswordHasher};
use crate::principal::AuthPrincipal;
use crate::session::{Session, SessionError, Sessions};
use crate::throttle::{MemoryThrottleStore, ThrottleStore, elapsed};

/// Default [`Totp::with_lockout`] failures: wrong codes before verification locks.
pub const DEFAULT_MAX_FAILURES: u32 = 5;
/// Default [`Totp::with_lockout`] window: how long a lockout lasts, and how far apart failures
/// may be to count together.
pub const DEFAULT_LOCKOUT: Duration = Duration::from_secs(15 * 60);
/// Posture id of [`Totp::dangerous_without_lockout`] (and the deprecated
/// [`Totp::without_lockout_dangerous`]).
pub const DANGEROUS_WITHOUT_LOCKOUT: &str = "id_effect_auth::totp::Totp::dangerous_without_lockout";

/// Bytes of CSPRNG output in a secret (160 bits, RFC 4226 §4's recommendation).
const SECRET_BYTES: usize = 20;

/// Session attribute [`Totp::step_up`] sets: the Unix time (seconds) of the last second factor.
pub const MFA_VERIFIED_AT_ATTR: &str = "mfa_verified_at";
/// Principal claim listing the authentication methods (RFC 8176 `amr`).
pub const AMR_CLAIM: &str = "amr";
/// Principal claim [`with_mfa_claims`] sets: the Unix time (seconds) of the last second factor.
pub const MFA_AT_CLAIM: &str = "mfa_at";

/// TOTP failures. A wrong or replayed code is not an error: it is a [`TotpVerification`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TotpError {
  /// The parameters, secret, issuer or account name are not acceptable to RFC 6238 / the
  /// `otpauth` URI format (fewer than 6 or more than 8 digits, a step of zero, a secret under
  /// 128 bits, a `:` in the issuer or account name, invalid base32).
  Config(String),
  /// The operating system's random source failed.
  Random(String),
  /// The [`TotpStepStore`] failed.
  Storage(String),
  /// Hashing or verifying a recovery code failed.
  Password(PasswordError),
  /// Too many wrong codes for the user: verification is refused, without checking the code,
  /// for `retry_after`.
  Locked {
    /// How long until the next attempt is allowed.
    retry_after: Duration,
  },
}

impl fmt::Display for TotpError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Config(m) => write!(f, "invalid TOTP configuration: {m}"),
      Self::Random(m) => write!(f, "TOTP secret generation: {m}"),
      Self::Storage(m) => write!(f, "TOTP step storage: {m}"),
      Self::Password(e) => write!(f, "recovery code hashing: {e}"),
      Self::Locked { retry_after } => write!(
        f,
        "too many wrong codes; retry in {} s",
        crate::error::retry_after_secs(*retry_after)
      ),
    }
  }
}

impl std::error::Error for TotpError {}

impl From<PasswordError> for TotpError {
  fn from(e: PasswordError) -> Self {
    Self::Password(e)
  }
}

fn random(bytes: &mut [u8]) -> Result<(), TotpError> {
  getrandom::fill(bytes).map_err(|e| TotpError::Random(e.to_string()))
}

/// A shared TOTP secret. `Debug` never shows it; store it encrypted at rest.
#[derive(Clone, PartialEq, Eq)]
pub struct TotpSecret(Secret);

impl TotpSecret {
  /// A fresh random 160-bit secret.
  ///
  /// # Errors
  ///
  /// [`TotpError::Random`] when the operating system's random source fails.
  pub fn generate() -> Result<Self, TotpError> {
    let mut bytes = [0_u8; SECRET_BYTES];
    random(&mut bytes)?;
    Ok(Self(Secret::from(bytes)))
  }

  /// A secret from its raw bytes (at least 16 are needed to verify codes).
  pub fn from_bytes(bytes: &[u8]) -> Self {
    Self(Secret::from(bytes))
  }

  /// A secret from its unpadded RFC 4648 base32 form (as stored, or as in an `otpauth` URI).
  ///
  /// # Errors
  ///
  /// [`TotpError::Config`] when `encoded` is not base32.
  pub fn from_base32(encoded: &str) -> Result<Self, TotpError> {
    Secret::try_from_base32(encoded)
      .map(Self)
      .map_err(|e| TotpError::Config(e.to_string()))
  }

  /// The unpadded base32 form, to store or to type into an authenticator app.
  pub fn to_base32(&self) -> String {
    self.0.to_base32()
  }

  /// The raw bytes.
  #[inline]
  pub fn as_bytes(&self) -> &[u8] {
    self.0.as_bytes()
  }
}

impl fmt::Debug for TotpSecret {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("TotpSecret(<redacted>)")
  }
}

/// Code parameters and the issuer shown in authenticator apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TotpConfig {
  /// The service name in the URI label and `issuer` parameter (no `:`).
  pub issuer: String,
  /// HMAC algorithm (default SHA-1: the one every authenticator app implements).
  pub algorithm: Algorithm,
  /// Code length, 6 to 8 (default 6).
  pub digits: u8,
  /// Seconds per step (default 30).
  pub step_secs: u64,
  /// Steps accepted either side of the current one for clock drift (default 1).
  pub skew: u16,
}

impl TotpConfig {
  /// The RFC 6238 defaults (SHA-1, 6 digits, 30 s steps, ±1 step) for `issuer`.
  pub fn new(issuer: impl Into<String>) -> Self {
    Self {
      issuer: issuer.into(),
      algorithm: Algorithm::SHA1,
      digits: 6,
      step_secs: 30,
      skew: 1,
    }
  }
}

/// The result of [`Totp::enrol`]: show the URI (as a QR code) or the base32 secret to the user
/// once, have them confirm with a first code, then store the secret. `Debug` shows neither.
#[derive(Clone)]
pub struct Enrolment {
  secret: TotpSecret,
  uri: String,
}

impl fmt::Debug for Enrolment {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Enrolment(<redacted>)")
  }
}

impl Enrolment {
  /// The new secret.
  #[inline]
  pub fn secret(&self) -> &TotpSecret {
    &self.secret
  }

  /// The `otpauth://totp/<issuer>:<account>?secret=…&issuer=…` URI.
  #[inline]
  pub fn uri(&self) -> &str {
    &self.uri
  }
}

/// The outcome of [`Totp::verify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TotpVerification {
  /// The code matches a step within the skew window that the user had not used yet; that step
  /// is now consumed.
  Valid,
  /// The code matches no step within the window.
  Invalid,
  /// The code matches a step the user already consumed (or one before it): a replay.
  Replayed,
}

impl TotpVerification {
  /// `true` only for [`Valid`](Self::Valid).
  #[inline]
  pub fn is_valid(self) -> bool {
    self == Self::Valid
  }
}

/// The last TOTP step each user consumed, for replay protection. Every method is an
/// [`Effect`], so a store may do I/O.
pub trait TotpStepStore: Send + Sync + 'static {
  /// Record `step` as `user_id`'s last consumed step if it is later than the recorded one, as one
  /// atomic compare-and-set; `true` when it was recorded, `false` when `step` was not later.
  fn consume(&self, user_id: &str, step: u64) -> Effect<bool, TotpError>;
}

#[derive(Default)]
struct MemorySteps {
  last: HashMap<String, u64>,
  fail: Option<String>,
}

/// The posture registry id [`MemoryTotpStepStore`] records when one is created (0.11 M18): its
/// replay state is memory-only.
pub const MEMORY_TOTP_STEP_STORE_POSTURE: &str = "id_effect_auth::MemoryTotpStepStore::memory_only";

/// In-memory [`TotpStepStore`] for tests and single-process development. Clones share the
/// records.
///
/// Each instance of a service remembers only the codes it accepted itself, so behind a load
/// balancer a code accepted on one instance is accepted again on another (M18). Creating one
/// records [`MEMORY_TOTP_STEP_STORE_POSTURE`] in the posture registry, which the prod posture
/// check reports as memory-only security state; use `PgTotpStepStore` there.
#[derive(Clone)]
pub struct MemoryTotpStepStore {
  inner: Arc<Mutex<MemorySteps>>,
}

impl Default for MemoryTotpStepStore {
  fn default() -> Self {
    id_effect::observability::posture::record_dangerous(
      MEMORY_TOTP_STEP_STORE_POSTURE,
      "M18: TOTP replay state is memory-only; a code accepted on one instance is accepted again \
       on another",
    );
    Self {
      inner: Arc::default(),
    }
  }
}

impl fmt::Debug for MemoryTotpStepStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryTotpStepStore")
      .finish_non_exhaustive()
  }
}

impl MemoryTotpStepStore {
  /// An empty store.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  /// The last step `user_id` consumed.
  pub fn last_step(&self, user_id: &str) -> Option<u64> {
    let st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
    st.last.get(user_id).copied()
  }

  /// Make every later operation fail with [`TotpError::Storage`]`(message)` (a store outage),
  /// or recover with `None`.
  pub fn fail_with(&self, message: Option<&str>) {
    let mut st = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
    st.fail = message.map(str::to_owned);
  }
}

impl TotpStepStore for MemoryTotpStepStore {
  fn consume(&self, user_id: &str, step: u64) -> Effect<bool, TotpError> {
    let (inner, user_id) = (Arc::clone(&self.inner), user_id.to_owned());
    suspend(move || {
      let mut st = inner.lock().unwrap_or_else(PoisonError::into_inner);
      if let Some(m) = st.fail.clone() {
        return fail(TotpError::Storage(m));
      }
      let later = st.last.get(&user_id).is_none_or(|&last| step > last);
      if later {
        st.last.insert(user_id, step);
      }
      succeed(later)
    })
  }
}

#[cfg(feature = "pg")]
mod pg {
  use std::fmt;

  use id_effect::{Effect, fail, succeed};
  use id_effect_sql::{SqlClient, SqlClientService, SqlError, params};

  use super::{TotpError, TotpStepStore};
  use crate::session::statements;

  /// The crate's migration `migrations/0003_id_effect_auth_state.up.sql`: what
  /// [`PgTotpStepStore::ensure_schema`] runs.
  const SCHEMA: &str = include_str!("../migrations/0003_id_effect_auth_state.up.sql");

  /// The compare-and-set: inserts the first step, or moves it forward only when `$2` is later.
  /// The affected row count is 1 when the step was recorded, 0 when it was not later.
  pub(super) const CONSUME: &str = "INSERT INTO id_effect_totp_steps AS s (user_id, last_step) \
    VALUES ($1, $2) \
    ON CONFLICT (user_id) DO UPDATE SET last_step = EXCLUDED.last_step \
    WHERE s.last_step < EXCLUDED.last_step";

  /// [`TotpStepStore`] in the PostgreSQL table `id_effect_totp_steps`, over any [`SqlClient`]
  /// (`id_effect_sql::pg::PgSqlClient` in production): the replay state every instance shares,
  /// so a code is accepted once across all of them (M18). Clones share the client.
  ///
  /// [`consume`](TotpStepStore::consume) is one statement (`INSERT … ON CONFLICT (user_id) DO
  /// UPDATE … WHERE last_step < EXCLUDED.last_step`): of two instances consuming the same step
  /// at once, the database lets exactly one record it.
  ///
  /// Create the table with the crate's `migrations/0003_id_effect_auth_state.up.sql` or with
  /// [`ensure_schema`](Self::ensure_schema). Driver failures surface as
  /// [`TotpError::Storage`].
  #[derive(Clone)]
  pub struct PgTotpStepStore {
    client: SqlClientService,
  }

  impl fmt::Debug for PgTotpStepStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
      f.debug_struct("PgTotpStepStore")
        .field("table", &"id_effect_totp_steps")
        .finish_non_exhaustive()
    }
  }

  impl PgTotpStepStore {
    /// A store over `client`.
    #[inline]
    pub fn new(client: impl SqlClient) -> Self {
      Self::from_service(SqlClientService::new(client))
    }

    /// A store over the client capability `client` (shares the client and its pool).
    #[inline]
    pub fn from_service(client: SqlClientService) -> Self {
      Self { client }
    }

    /// Create the tables of `migrations/0003_id_effect_auth_state.up.sql` (this store's and
    /// `PgThrottleStore`'s) unless they exist, one statement at a time.
    pub fn ensure_schema(&self) -> Effect<(), TotpError> {
      let client = self.client.clone();
      statements(SCHEMA)
        .into_iter()
        .fold(succeed(()), move |done, sql| {
          let client = client.clone();
          done.flat_map(move |()| client.execute(&sql, &[]).map(|_| ()).map_err(storage))
        })
    }
  }

  fn storage(e: SqlError) -> TotpError {
    TotpError::Storage(e.to_string())
  }

  impl TotpStepStore for PgTotpStepStore {
    fn consume(&self, user_id: &str, step: u64) -> Effect<bool, TotpError> {
      let Ok(step) = i64::try_from(step) else {
        return fail(TotpError::Storage(format!("TOTP step {step} out of range")));
      };
      self
        .client
        .execute(CONSUME, &params![user_id, step])
        .map(|n| n > 0)
        .map_err(storage)
    }
  }
}

#[cfg(feature = "pg")]
pub use pg::PgTotpStepStore;

/// Failure counting for [`Totp::verify`].
#[derive(Clone)]
struct Lockout {
  failures: Arc<dyn ThrottleStore>,
  max_failures: u32,
  window: Duration,
}

/// TOTP enrolment and verification. Cheap to clone; clones share the failure counts.
#[derive(Clone)]
pub struct Totp {
  config: Arc<TotpConfig>,
  steps: Arc<dyn TotpStepStore>,
  clock: WallClock,
  lockout: Option<Lockout>,
}

impl fmt::Debug for Totp {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Totp")
      .field("config", &self.config)
      .field("clock", &self.clock)
      .finish_non_exhaustive()
  }
}

impl Totp {
  /// TOTP with `config`, consumed steps in `steps`, on the system clock, locking after
  /// [`DEFAULT_MAX_FAILURES`] wrong codes for [`DEFAULT_LOCKOUT`] (counted in memory: it records
  /// [`MEMORY_THROTTLE_STORE_POSTURE`](crate::MEMORY_THROTTLE_STORE_POSTURE), even when
  /// [`with_failure_store`](Self::with_failure_store) replaces the store; see
  /// [`with_stores`](Self::with_stores)).
  pub fn new(config: TotpConfig, steps: impl TotpStepStore) -> Self {
    Self::with_stores(config, steps, MemoryThrottleStore::new())
  }

  /// TOTP with `config`, consumed steps in `steps` and wrong codes counted in `failures` (shared
  /// by every instance, for example), on the system clock, locking after
  /// [`DEFAULT_MAX_FAILURES`] wrong codes for [`DEFAULT_LOCKOUT`]. No default in-memory store is
  /// created, so shared stores record nothing in the posture registry.
  pub fn with_stores(
    config: TotpConfig,
    steps: impl TotpStepStore,
    failures: impl ThrottleStore,
  ) -> Self {
    Self {
      config: Arc::new(config),
      steps: Arc::new(steps),
      clock: WallClock::system(),
      lockout: Some(Lockout {
        failures: Arc::new(failures),
        max_failures: DEFAULT_MAX_FAILURES,
        window: DEFAULT_LOCKOUT,
      }),
    }
  }

  /// The same TOTP locks a user's verification for `window` after `max_failures` wrong codes,
  /// each less than `window` after the one before (at least one failure).
  #[must_use]
  pub fn with_lockout(mut self, max_failures: u32, window: Duration) -> Self {
    let failures = match self.lockout.take() {
      Some(l) => l.failures,
      None => Arc::new(MemoryThrottleStore::new()),
    };
    self.lockout = Some(Lockout {
      failures,
      max_failures: max_failures.max(1),
      window,
    });
    self
  }

  /// The same TOTP counts wrong codes in `store` (shared by every instance, for example)
  /// instead of in memory.
  #[must_use]
  pub fn with_failure_store(mut self, store: impl ThrottleStore) -> Self {
    let (max_failures, window) = self
      .lockout
      .as_ref()
      .map_or((DEFAULT_MAX_FAILURES, DEFAULT_LOCKOUT), |l| {
        (l.max_failures, l.window)
      });
    self.lockout = Some(Lockout {
      failures: Arc::new(store),
      max_failures,
      window,
    });
    self
  }

  /// The same TOTP without the failure lockout. Records [`DANGEROUS_WITHOUT_LOCKOUT`] in the
  /// posture registry.
  ///
  /// **Dangerous**: it re-opens unlimited guessing of the second factor (about one success per
  /// 330,000 tries). Only for a deployment that limits TOTP attempts in front of
  /// [`verify`](Self::verify).
  #[must_use]
  pub fn dangerous_without_lockout(mut self) -> Self {
    id_effect::observability::posture::record_dangerous(
      DANGEROUS_WITHOUT_LOCKOUT,
      "H6: TOTP verification does not lock after failures, so second-factor guessing is \
       unlimited",
    );
    self.lockout = None;
    self
  }

  /// The same TOTP without the failure lockout.
  #[deprecated(
    since = "0.11.0",
    note = "renamed to dangerous_without_lockout (ADR 0013 naming)"
  )]
  #[must_use]
  pub fn without_lockout_dangerous(self) -> Self {
    self.dangerous_without_lockout()
  }

  /// The same TOTP reads time from `clock` (an anchored clock follows a `TestExecutor`'s virtual
  /// time).
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// The parameters.
  #[inline]
  pub fn config(&self) -> &TotpConfig {
    &self.config
  }

  fn rfc(&self, secret: &TotpSecret, account: &str) -> Result<totp_rs::Totp, TotpError> {
    let c = &self.config;
    Builder::new()
      .with_algorithm(c.algorithm)
      .with_digits(c.digits)
      .with_step_duration(c.step_secs)
      .with_skew(c.skew)
      .with_secret(secret.0.clone())
      .with_issuer(Some(c.issuer.as_str()))
      .with_account_name(account)
      .build()
      .map_err(|e| TotpError::Config(e.to_string()))
  }

  /// A fresh secret for `account` (the name the app shows, such as an email address) and its
  /// `otpauth://` URI.
  ///
  /// # Errors
  ///
  /// [`TotpError::Config`] for invalid parameters or a `:` in the issuer or account name;
  /// [`TotpError::Random`] when the random source fails.
  pub fn enrol(&self, account: &str) -> Result<Enrolment, TotpError> {
    let secret = TotpSecret::generate()?;
    let uri = self.uri(&secret, account)?;
    Ok(Enrolment { secret, uri })
  }

  /// The `otpauth://` URI of `secret` for `account`.
  ///
  /// # Errors
  ///
  /// [`TotpError::Config`] for invalid parameters, a secret under 128 bits, or a `:` in the
  /// issuer or account name.
  pub fn uri(&self, secret: &TotpSecret, account: &str) -> Result<String, TotpError> {
    self
      .rfc(secret, account)?
      .to_url()
      .map_err(|e| TotpError::Config(e.to_string()))
  }

  /// The code for `secret` at `at` (what an authenticator app shows then).
  ///
  /// # Errors
  ///
  /// [`TotpError::Config`] for invalid parameters or a secret under 128 bits.
  pub fn code_at(&self, secret: &TotpSecret, at: UtcDateTime) -> Result<String, TotpError> {
    Ok(self.rfc(secret, "")?.generate(unix_secs(at)).to_string())
  }

  /// Check `code` for `user_id`'s `secret` at the clock's current time: valid for the current
  /// step ± [`TotpConfig::skew`], and only for a step later than the last one `user_id`
  /// consumed (which it then consumes). Spaces in `code` are ignored.
  ///
  /// A wrong code counts as a failure for `user_id`; a valid one clears the count. Once
  /// `user_id` has too many (see the [module docs](self)), verification fails with
  /// [`TotpError::Locked`] without checking the code.
  pub fn verify(
    &self,
    user_id: &str,
    secret: &TotpSecret,
    code: &str,
  ) -> Effect<TotpVerification, TotpError> {
    let totp = match self.rfc(secret, "") {
      Ok(t) => t,
      Err(e) => return fail(e),
    };
    let code: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let (steps, user_id) = (Arc::clone(&self.steps), user_id.to_owned());
    let lockout = self.lockout.clone();
    self.clock.now().flat_map(move |now| {
      let key = format!("totp:{user_id}");
      let allowed = match &lockout {
        Some(l) => l.check(&key, now),
        None => succeed(()),
      };
      allowed.flat_map(move |()| {
        let checked = match totp.check(&code, unix_secs(now)) {
          None => succeed(TotpVerification::Invalid),
          Some(counter) => steps.consume(&user_id, counter).map(|fresh| {
            if fresh {
              TotpVerification::Valid
            } else {
              TotpVerification::Replayed
            }
          }),
        };
        checked.flat_map(move |outcome| match (lockout, outcome) {
          (Some(l), TotpVerification::Invalid) => l.record(&key, now).map(move |()| outcome),
          (Some(l), TotpVerification::Valid) => l
            .failures
            .clear(&key)
            .map_err(storage)
            .map(move |()| outcome),
          _ => succeed(outcome),
        })
      })
    })
  }

  /// Mark `session` as having passed a second factor now ([`MFA_VERIFIED_AT_ATTR`]), save it
  /// and give it a fresh id (a privilege change). `None` when the session is no longer live.
  /// Send the result's [`Sessions::set_cookie`].
  pub fn step_up(
    &self,
    sessions: &Sessions,
    mut session: Session,
  ) -> Effect<Option<Session>, SessionError> {
    let (sessions, clock, id) = (sessions.clone(), self.clock.clone(), session.id().clone());
    // Checked first, so saving the mark cannot bring back a session that ended meanwhile.
    sessions.load(&id).flat_map(move |live| {
      if live.is_none() {
        return succeed(None);
      }
      clock.now().flat_map(move |now| {
        mark_mfa(&mut session, now);
        let rotate = sessions.clone();
        sessions
          .save(session)
          .flat_map(move |()| rotate.rotate(&id))
      })
    })
  }
}

fn storage(e: crate::AuthError) -> TotpError {
  TotpError::Storage(e.to_string())
}

impl Lockout {
  /// Fails with [`TotpError::Locked`] while `key` has too many recent failures.
  fn check(&self, key: &str, now: UtcDateTime) -> Effect<(), TotpError> {
    let (max, window) = (self.max_failures, self.window);
    self
      .failures
      .get(key)
      .map_err(storage)
      .flat_map(move |found| match found {
        Some(f) if f.count >= max && elapsed(f.last, now) < window => fail(TotpError::Locked {
          retry_after: window.saturating_sub(elapsed(f.last, now)),
        }),
        _ => succeed(()),
      })
  }

  fn record(&self, key: &str, now: UtcDateTime) -> Effect<(), TotpError> {
    self
      .failures
      .record_failure(key, now, self.window)
      .map_err(storage)
      .map(|_| ())
  }
}

/// Seconds since the Unix epoch (0 before it).
fn unix_secs(t: UtcDateTime) -> u64 {
  u64::try_from(t.0.as_second()).unwrap_or(0)
}

/// Set [`MFA_VERIFIED_AT_ATTR`] on `session` to `at`; persist it with [`Sessions::save`]
/// ([`Totp::step_up`] does both, and rotates the id).
pub fn mark_mfa(session: &mut Session, at: UtcDateTime) {
  session.set_attr(MFA_VERIFIED_AT_ATTR, at.0.as_second().to_string());
}

/// When `session` last passed a second factor, if it did.
pub fn mfa_verified_at(session: &Session) -> Option<UtcDateTime> {
  let secs: i64 = session.attr(MFA_VERIFIED_AT_ATTR)?.parse().ok()?;
  jiff::Timestamp::from_second(secs).ok().map(UtcDateTime)
}

/// `true` when `session` passed a second factor no longer than `max_age` before `now` (for
/// re-prompting before a sensitive action).
pub fn is_mfa_fresh(session: &Session, now: UtcDateTime, max_age: Duration) -> bool {
  mfa_verified_at(session).is_some_and(|at| at <= now && at.distance_duration(&now) <= max_age)
}

/// `principal` with the MFA claims of `session` when it passed a second factor: `amr` gains
/// `otp` and `mfa` (kept as an array, merged with any methods already there) and
/// [`MFA_AT_CLAIM`] holds the time in Unix seconds. Unchanged otherwise.
pub fn with_mfa_claims(principal: AuthPrincipal, session: &Session) -> AuthPrincipal {
  let Some(at) = mfa_verified_at(session) else {
    return principal;
  };
  let mut amr = match principal.claims().get(AMR_CLAIM) {
    Some(Value::Array(methods)) => methods.clone(),
    Some(other) => vec![other.clone()],
    None => Vec::new(),
  };
  for method in ["otp", "mfa"] {
    let method = Value::from(method);
    if !amr.contains(&method) {
      amr.push(method);
    }
  }
  principal
    .with_claim(AMR_CLAIM, Value::Array(amr))
    .with_claim(MFA_AT_CLAIM, at.0.as_second())
}

/// An `id_effect_authz` policy allowing principals whose `amr` claim contains `mfa` (see
/// [`with_mfa_claims`]; a JWT from an identity provider that performed MFA carries it too).
pub fn require_mfa() -> ClaimPolicy<ClaimContains> {
  claim_contains(AMR_CLAIM, "mfa")
}

/// Symbols in a recovery code: base32 without `I`, `O`, `0` and `1`, so codes read unambiguously.
const RECOVERY_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
/// Symbols per recovery code (10 × 5 bits = 50 bits).
const RECOVERY_LEN: usize = 10;

/// A plaintext recovery code (`XXXXX-XXXXX`), to show the user once. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryCode(String);

impl RecoveryCode {
  fn generate() -> Result<Self, TotpError> {
    let mut bytes = [0_u8; RECOVERY_LEN];
    random(&mut bytes)?;
    let symbols: Vec<char> = bytes
      .iter()
      .map(|b| char::from(RECOVERY_ALPHABET[usize::from(b & 31)]))
      .collect();
    let (a, b) = symbols.split_at(RECOVERY_LEN / 2);
    Ok(Self(format!(
      "{}-{}",
      a.iter().collect::<String>(),
      b.iter().collect::<String>()
    )))
  }

  /// The code as the user sees it.
  #[inline]
  pub fn expose(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for RecoveryCode {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("RecoveryCode(<redacted>)")
  }
}

/// A code as typed: upper-cased, with separators and spaces removed (what is hashed).
fn normalize(code: &str) -> String {
  code
    .chars()
    .filter(|c| c.is_ascii_alphanumeric())
    .map(|c| c.to_ascii_uppercase())
    .collect()
}

/// Freshly generated recovery codes: show [`codes`](Self::codes) once, store
/// [`hashes`](Self::hashes).
#[derive(Clone, Debug)]
pub struct GeneratedRecoveryCodes {
  /// The plaintext codes.
  pub codes: Vec<RecoveryCode>,
  /// Their argon2id hashes, in the same order.
  pub hashes: Vec<PasswordHash>,
}

/// The outcome of [`RecoveryCodes::redeem`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Redemption {
  /// The code matched one stored hash; store `remaining` (the others) in place of the old set,
  /// so the code cannot be used again.
  Redeemed {
    /// The hashes still unused.
    remaining: Vec<PasswordHash>,
  },
  /// The code matched no stored hash.
  Invalid,
}

/// Single-use recovery codes, stored as argon2id hashes. Cheap to clone.
#[derive(Clone, Debug, Default)]
pub struct RecoveryCodes {
  hasher: PasswordHasher,
}

impl RecoveryCodes {
  /// Recovery codes hashed by `hasher`.
  pub fn new(hasher: PasswordHasher) -> Self {
    Self { hasher }
  }

  /// `count` fresh codes and their hashes (hashed on the blocking pool). Replace any earlier
  /// set with them.
  pub fn generate(&self, count: usize) -> Effect<GeneratedRecoveryCodes, TotpError> {
    let codes = match (0..count)
      .map(|_| RecoveryCode::generate())
      .collect::<Result<Vec<_>, _>>()
    {
      Ok(codes) => codes,
      Err(e) => return fail(e),
    };
    let mut hashes: Effect<Vec<PasswordHash>, PasswordError> = succeed(Vec::new());
    for code in &codes {
      let (hasher, plain) = (self.hasher.clone(), normalize(code.expose()));
      hashes = hashes.flat_map(move |mut acc| {
        hasher.hash(plain).map(move |h| {
          acc.push(h);
          acc
        })
      });
    }
    hashes
      .map(move |hashes| GeneratedRecoveryCodes { codes, hashes })
      .map_err(TotpError::Password)
  }

  /// Check `code` against every hash in `stored`, all of them whatever matches, so the time
  /// taken does not reveal which one did. A match yields the other hashes to store in place of
  /// `stored`.
  pub fn redeem(&self, code: &str, stored: Vec<PasswordHash>) -> Effect<Redemption, TotpError> {
    let plain = normalize(code);
    let mut found: Effect<Option<usize>, PasswordError> = succeed(None);
    for (i, hash) in stored.iter().enumerate() {
      let (hasher, plain, hash) = (self.hasher.clone(), plain.clone(), hash.clone());
      found = found.flat_map(move |prev| {
        hasher
          .verify(plain, &hash)
          .map(move |v| prev.or(v.is_valid().then_some(i)))
      });
    }
    found
      .map(move |hit| match hit {
        Some(i) => {
          let mut remaining = stored;
          remaining.remove(i);
          Redemption::Redeemed { remaining }
        }
        None => Redemption::Invalid,
      })
      .map_err(TotpError::Password)
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::time::Instant;

  use id_effect::{Cause, Exit};
  use id_effect_authz::Principal;

  use super::*;
  use crate::session::{MemorySessionStore, SessionConfig, SessionId};

  fn run<A: Send + 'static, E: Send + fmt::Debug + 'static>(eff: Effect<A, E>) -> Result<A, E> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn at(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  fn fixture() -> (Totp, MemoryTotpStepStore, WallClock) {
    let steps = MemoryTotpStepStore::new();
    let clock = WallClock::anchored(Instant::now(), at(1_800_000_000));
    let totp = Totp::new(TotpConfig::new("Acme"), steps.clone()).with_clock(clock.clone());
    (totp, steps, clock)
  }

  fn session() -> Session {
    Session::from_parts(
      SessionId::generate().unwrap(),
      "u",
      BTreeMap::new(),
      at(0),
      at(0),
      at(1),
      at(1),
    )
  }

  #[test]
  fn secrets_are_160_random_bits_and_round_trip_base32() {
    let a = TotpSecret::generate().unwrap();
    let b = TotpSecret::generate().unwrap();
    assert_ne!(a, b);
    assert_eq!(a.as_bytes().len(), 20);
    assert_eq!(a.to_base32().len(), 32);
    assert_eq!(TotpSecret::from_base32(&a.to_base32()).unwrap(), a);
    assert!(matches!(
      TotpSecret::from_base32("not base32!"),
      Err(TotpError::Config(_))
    ));
    assert_eq!(format!("{a:?}"), "TotpSecret(<redacted>)");
  }

  #[test]
  fn invalid_parameters_are_config_errors() {
    let mut config = TotpConfig::new("Acme");
    config.digits = 5;
    let totp = Totp::new(config, MemoryTotpStepStore::new());
    let secret = TotpSecret::generate().unwrap();
    assert!(matches!(totp.enrol("a"), Err(TotpError::Config(_))));
    assert!(matches!(
      totp.code_at(&secret, at(0)),
      Err(TotpError::Config(_))
    ));
    assert!(matches!(
      run(totp.verify("u", &secret, "123456")),
      Err(TotpError::Config(_))
    ));
    let (ok, _, _) = fixture();
    assert!(matches!(ok.uri(&secret, "a:b"), Err(TotpError::Config(_))));
    assert!(matches!(
      ok.code_at(&TotpSecret::from_bytes(&[1; 10]), at(0)),
      Err(TotpError::Config(_))
    ));
  }

  #[test]
  fn verify_tolerates_spaces_and_rejects_garbage() {
    let (totp, steps, _) = fixture();
    let secret = TotpSecret::generate().unwrap();
    let code = totp.code_at(&secret, at(1_800_000_000)).unwrap();
    let spaced = format!("{} {}", &code[..3], &code[3..]);
    assert_eq!(
      run(totp.verify("u", &secret, &spaced)),
      Ok(TotpVerification::Valid)
    );
    assert_eq!(
      run(totp.verify("u", &secret, "abcdef")),
      Ok(TotpVerification::Invalid)
    );
    assert_eq!(steps.last_step("u"), Some(1_800_000_000 / 30));
    assert!(!TotpVerification::Invalid.is_valid());
    assert!(format!("{steps:?}").contains("MemoryTotpStepStore"));
  }

  #[test]
  fn h6_totp_locks_after_five_failures() {
    let (totp, _, clock) = fixture();
    let secret = TotpSecret::generate().unwrap();
    let now = 1_800_000_000;
    let wrong = |offset: i64| {
      // A code from far outside the skew window: well-formed, never valid now.
      totp.code_at(&secret, at(now - 3_600 + offset)).unwrap()
    };
    for i in 0..5 {
      assert_eq!(
        run(totp.verify("u", &secret, &wrong(i))),
        Ok(TotpVerification::Invalid)
      );
    }
    // Locked: even the right code is refused, and the code is not even checked.
    let right = totp.code_at(&secret, at(now)).unwrap();
    let Err(TotpError::Locked { retry_after }) = run(totp.verify("u", &secret, &right)) else {
      panic!("the sixth attempt must be locked")
    };
    assert!(
      retry_after <= DEFAULT_LOCKOUT && retry_after > DEFAULT_LOCKOUT - Duration::from_secs(5)
    );
    assert!(
      TotpError::Locked { retry_after }
        .to_string()
        .contains("too many wrong codes")
    );
    // Other users are not affected; clones share the count.
    assert_eq!(
      run(totp.verify("v", &secret, &right)),
      Ok(TotpVerification::Valid)
    );
    assert!(matches!(
      run(totp.clone().verify("u", &secret, &right)),
      Err(TotpError::Locked { .. })
    ));
    // The lockout ends; a valid code then clears the count.
    clock.jump(DEFAULT_LOCKOUT);
    let later = totp.code_at(&secret, at(now + 900)).unwrap();
    assert_eq!(
      run(totp.verify("u", &secret, &later)),
      Ok(TotpVerification::Valid)
    );
    for i in 0..4 {
      assert_eq!(
        run(totp.verify("u", &secret, &wrong(i))),
        Ok(TotpVerification::Invalid)
      );
    }
    assert_eq!(
      run(totp.verify("u", &secret, &wrong(9))),
      Ok(TotpVerification::Invalid),
      "the count started over after the valid code"
    );
  }

  #[test]
  fn lockout_can_be_tuned_moved_or_dropped() {
    let (totp, _, _) = fixture();
    let secret = TotpSecret::generate().unwrap();
    let wrong = totp.code_at(&secret, at(1_000)).unwrap();
    let strict = totp
      .clone()
      .with_failure_store(MemoryThrottleStore::new())
      .with_lockout(1, Duration::from_secs(60));
    assert_eq!(
      run(strict.verify("u", &secret, &wrong)),
      Ok(TotpVerification::Invalid)
    );
    assert!(matches!(
      run(strict.verify("u", &secret, &wrong)),
      Err(TotpError::Locked { .. })
    ));
    let open = strict.dangerous_without_lockout();
    for _ in 0..10 {
      assert_eq!(
        run(open.verify("u", &secret, &wrong)),
        Ok(TotpVerification::Invalid)
      );
    }
    let back = open.with_failure_store(MemoryThrottleStore::new());
    assert_eq!(
      run(back.verify("u", &secret, &wrong)),
      Ok(TotpVerification::Invalid)
    );
    let relocked = back
      .dangerous_without_lockout()
      .with_lockout(0, Duration::from_secs(60));
    assert_eq!(
      run(relocked.verify("u", &secret, &wrong)),
      Ok(TotpVerification::Invalid)
    );
    assert!(matches!(
      run(relocked.verify("u", &secret, &wrong)),
      Err(TotpError::Locked { .. })
    ));
  }

  #[test]
  fn step_store_failures_surface() {
    let (totp, steps, _) = fixture();
    let secret = TotpSecret::generate().unwrap();
    let code = totp.code_at(&secret, at(1_800_000_000)).unwrap();
    steps.fail_with(Some("down"));
    assert_eq!(
      run(totp.verify("u", &secret, &code)),
      Err(TotpError::Storage("down".into()))
    );
    steps.fail_with(None);
    assert_eq!(
      run(totp.verify("u", &secret, &code)),
      Ok(TotpVerification::Valid)
    );
  }

  #[test]
  fn errors_display() {
    let cases = [
      (TotpError::Config("x".into()), "configuration: x"),
      (TotpError::Random("x".into()), "generation: x"),
      (TotpError::Storage("x".into()), "storage: x"),
      (
        TotpError::from(PasswordError::Blocking),
        "recovery code hashing",
      ),
    ];
    for (e, text) in cases {
      assert!(e.to_string().contains(text), "{e}");
    }
  }

  #[test]
  fn before_the_epoch_counts_as_zero() {
    assert_eq!(unix_secs(UtcDateTime::unsafe_make(-5_000)), 0);
  }

  #[test]
  fn mfa_marks_and_claims() {
    let mut s = session();
    assert_eq!(mfa_verified_at(&s), None);
    let p = AuthPrincipal::new("u");
    assert_eq!(with_mfa_claims(p.clone(), &s), p);
    mark_mfa(&mut s, at(1_000));
    assert_eq!(mfa_verified_at(&s), Some(at(1_000)));
    assert!(is_mfa_fresh(&s, at(1_300), Duration::from_secs(300)));
    assert!(!is_mfa_fresh(&s, at(1_301), Duration::from_secs(300)));
    assert!(!is_mfa_fresh(&s, at(999), Duration::from_secs(300)));
    let p = with_mfa_claims(AuthPrincipal::new("u").with_claim(AMR_CLAIM, "pwd"), &s);
    assert_eq!(
      p.claim(AMR_CLAIM),
      Some(&serde_json::json!(["pwd", "otp", "mfa"]))
    );
    assert_eq!(p.claim(MFA_AT_CLAIM), Some(&serde_json::json!(1_000)));
    let twice = with_mfa_claims(p.clone(), &s);
    assert_eq!(twice.claim(AMR_CLAIM), p.claim(AMR_CLAIM));
    s.set_attr(MFA_VERIFIED_AT_ATTR, "garbage");
    assert_eq!(mfa_verified_at(&s), None);
  }

  #[test]
  fn step_up_marks_and_rotates_the_session() {
    let (totp, _, clock) = fixture();
    let sessions =
      Sessions::new(MemorySessionStore::new(), SessionConfig::default()).with_clock(clock.clone());
    let s = run(sessions.login("u", None)).unwrap();
    clock.jump(Duration::from_secs(5));
    let up = run(totp.step_up(&sessions, s.clone()))
      .unwrap()
      .expect("live");
    assert_ne!(up.id(), s.id());
    assert_eq!(mfa_verified_at(&up), Some(at(1_800_000_005)));
    assert_eq!(run(sessions.load(s.id())).unwrap(), None);
    assert!(format!("{totp:?}").contains("Acme"));
    assert_eq!(totp.config().digits, 6);
  }

  #[test]
  fn recovery_codes_have_the_expected_shape() {
    let code = RecoveryCode::generate().unwrap();
    let text = code.expose();
    assert_eq!(text.len(), 11);
    assert_eq!(&text[5..6], "-");
    assert!(
      normalize(text)
        .bytes()
        .all(|b| RECOVERY_ALPHABET.contains(&b))
    );
    assert_eq!(format!("{code:?}"), "RecoveryCode(<redacted>)");
    assert_eq!(normalize(" abcde-fghij "), "ABCDEFGHIJ");
  }

  #[test]
  fn h6_dangerous_without_lockout_is_recorded() {
    let (totp, _, _) = fixture();
    #[allow(deprecated)]
    let shim = totp.clone().without_lockout_dangerous();
    assert!(shim.lockout.is_none());
    let open = totp.dangerous_without_lockout();
    assert!(open.lockout.is_none());
    let found = id_effect::observability::posture::dangerous_opt_outs();
    let (_, reopens) = found
      .iter()
      .find(|(id, _)| *id == DANGEROUS_WITHOUT_LOCKOUT)
      .expect("recorded");
    assert!(reopens.starts_with("H6:"));
  }

  #[test]
  fn m18_memory_step_store_is_reported_as_memory_only() {
    let _ = MemoryTotpStepStore::default();
    let found = id_effect::observability::posture::dangerous_opt_outs();
    let (_, reopens) = found
      .iter()
      .find(|(id, _)| *id == MEMORY_TOTP_STEP_STORE_POSTURE)
      .expect("recorded");
    assert!(reopens.starts_with("M18: TOTP replay state is memory-only"));
  }

  #[cfg(feature = "pg")]
  #[test]
  fn the_pg_step_store_consumes_with_one_compare_and_set() {
    use id_effect_sql::{SqlClient, SqlValue, TestSqlClient};

    let client = TestSqlClient::new();
    let store = PgTotpStepStore::new(client.clone());
    assert!(matches!(
      run(store.consume("u", 7)),
      Err(TotpError::Storage(_))
    ));
    assert!(matches!(
      run(store.ensure_schema()),
      Err(TotpError::Storage(_))
    ));
    run(client.connect()).unwrap();
    assert_eq!(run(store.consume("u", 7)), Ok(false), "no row affected");
    client.script_execute(pg::CONSUME, 1);
    assert_eq!(run(store.consume("u", 8)), Ok(true));
    let (sql, params) = &client.recorded_executes()[1];
    assert_eq!(sql, pg::CONSUME);
    assert_eq!(params, &[SqlValue::Text("u".into()), SqlValue::I64(8)]);
    assert!(matches!(
      run(store.consume("u", u64::MAX)),
      Err(TotpError::Storage(m)) if m.contains("out of range")
    ));
    run(store.ensure_schema()).unwrap();
    let ddl: Vec<String> = client
      .recorded_executes()
      .into_iter()
      .skip(2)
      .map(|(sql, _)| sql)
      .collect();
    assert_eq!(ddl.len(), 3, "{ddl:?}");
    assert!(ddl[0].starts_with("CREATE TABLE IF NOT EXISTS id_effect_totp_steps"));
    assert!(format!("{store:?}").contains("id_effect_totp_steps"));
  }
}
