//! [`LoginThrottle`]: exponential backoff after failed logins, per account and per client
//! address, checked before any password hashing.
//!
//! Every failed attempt is counted under two keys: the account (the login name, lower-cased) and
//! the client address, when the caller knows it. The first
//! [`free_attempts`](LoginThrottle::free_attempts) failures of a key (5 by default) cost
//! nothing; after that the key is locked from its last failure for a delay that doubles with
//! every further failure, from [`base_delay`](LoginThrottle::base_delay) (1 s) up to
//! [`max_delay`](LoginThrottle::max_delay) (15 minutes). An attempt while either key is locked
//! fails with [`AuthError::TooManyAttempts`] (HTTP 429 with `Retry-After`) before the user is
//! looked up or a password is hashed.
//!
//! A successful login clears the account's count. It does not clear the address's: one valid
//! account must not reset an address's budget for guessing the passwords of others. A key's
//! failures are forgotten once [`forget_after`](LoginThrottle::forget_after) (1 hour) passes
//! without a new one.
//!
//! The counts live in a [`ThrottleStore`]. [`MemoryThrottleStore`] (the default) counts per
//! process; several instances behind a load balancer each count on their own until a shared
//! store is plugged in. [`BasicAuth`](crate::BasicAuth) throttles with a default
//! `LoginThrottle`; an application's own login calls [`check`](LoginThrottle::check) before
//! hashing and [`record_failure`](LoginThrottle::record_failure) /
//! [`record_success`](LoginThrottle::record_success) after.
//!
//! ```
//! use std::net::{IpAddr, Ipv4Addr};
//!
//! use id_effect::{Cause, Exit};
//! use id_effect_auth::{AuthError, LoginThrottle};
//!
//! let throttle = LoginThrottle::default().free_attempts(1);
//! let client = Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)));
//! assert!(matches!(throttle.check("alice", client).block_on(), Exit::Success(())));
//! let _ = throttle.record_failure("alice", client).block_on();
//! assert!(matches!(
//!   throttle.check("alice", client).block_on(),
//!   Exit::Failure(Cause::Fail(AuthError::TooManyAttempts { .. }))
//! ));
//! ```

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use id_effect::{Effect, UtcDateTime, WallClock, fail, succeed, suspend};

use crate::error::AuthError;

/// Default [`LoginThrottle::free_attempts`].
pub const DEFAULT_FREE_ATTEMPTS: u32 = 5;
/// Default [`LoginThrottle::base_delay`].
pub const DEFAULT_BASE_DELAY: Duration = Duration::from_secs(1);
/// Default [`LoginThrottle::max_delay`].
pub const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// Default [`LoginThrottle::forget_after`].
pub const DEFAULT_FORGET_AFTER: Duration = Duration::from_secs(60 * 60);

/// The failures counted under one key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failures {
  /// How many failures in a row, none further apart than the store was told to forget after.
  pub count: u32,
  /// When the last one happened.
  pub last: UtcDateTime,
}

/// The time from `earlier` to `later` (zero when `later` is not after it).
pub(crate) fn elapsed(earlier: UtcDateTime, later: UtcDateTime) -> Duration {
  if later <= earlier {
    return Duration::ZERO;
  }
  Duration::try_from(later.0.duration_since(earlier.0)).unwrap_or(Duration::ZERO)
}

/// Where failure counts live. Every method is an [`Effect`], so a store may do I/O (a table, a
/// Redis key with a TTL).
pub trait ThrottleStore: Send + Sync + 'static {
  /// The failures counted under `key`, if any.
  fn get(&self, key: &str) -> Effect<Option<Failures>, AuthError>;

  /// Count one more failure under `key` at `at`, as one atomic step, and return the new count.
  /// A count whose last failure is more than `forget_after` before `at` starts over at one.
  fn record_failure(
    &self,
    key: &str,
    at: UtcDateTime,
    forget_after: Duration,
  ) -> Effect<Failures, AuthError>;

  /// Forget the failures under `key`.
  fn clear(&self, key: &str) -> Effect<(), AuthError>;
}

/// Above this many keys, [`MemoryThrottleStore`] drops forgotten ones on the next failure.
const MEMORY_PRUNE_AT: usize = 10_000;

/// In-memory [`ThrottleStore`] (per process). Clones share the counts. Keys whose failures are
/// forgotten are pruned as the store grows, so a spray of made-up names cannot grow it without
/// bound.
#[derive(Clone, Default)]
pub struct MemoryThrottleStore {
  inner: Arc<Mutex<HashMap<String, Failures>>>,
}

impl fmt::Debug for MemoryThrottleStore {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("MemoryThrottleStore")
      .field("keys", &self.state().len())
      .finish()
  }
}

impl MemoryThrottleStore {
  /// An empty store.
  #[inline]
  pub fn new() -> Self {
    Self::default()
  }

  fn state(&self) -> MutexGuard<'_, HashMap<String, Failures>> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl ThrottleStore for MemoryThrottleStore {
  fn get(&self, key: &str) -> Effect<Option<Failures>, AuthError> {
    let (this, key) = (self.clone(), key.to_owned());
    suspend(move || succeed(this.state().get(&key).copied()))
  }

  fn record_failure(
    &self,
    key: &str,
    at: UtcDateTime,
    forget_after: Duration,
  ) -> Effect<Failures, AuthError> {
    let (this, key) = (self.clone(), key.to_owned());
    suspend(move || {
      let mut st = this.state();
      if st.len() >= MEMORY_PRUNE_AT {
        st.retain(|_, f| elapsed(f.last, at) <= forget_after);
      }
      let count = match st.get(&key) {
        Some(f) if elapsed(f.last, at) <= forget_after => f.count.saturating_add(1),
        _ => 1,
      };
      let failures = Failures { count, last: at };
      st.insert(key, failures);
      succeed(failures)
    })
  }

  fn clear(&self, key: &str) -> Effect<(), AuthError> {
    let (this, key) = (self.clone(), key.to_owned());
    suspend(move || {
      this.state().remove(&key);
      succeed(())
    })
  }
}

/// Failed-login backoff per account and per client address. Cheap to clone; clones share the
/// store. See the [module documentation](self).
#[derive(Clone)]
pub struct LoginThrottle {
  store: Arc<dyn ThrottleStore>,
  free_attempts: u32,
  base_delay: Duration,
  max_delay: Duration,
  forget_after: Duration,
  clock: WallClock,
}

impl fmt::Debug for LoginThrottle {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("LoginThrottle")
      .field("free_attempts", &self.free_attempts)
      .field("base_delay", &self.base_delay)
      .field("max_delay", &self.max_delay)
      .field("forget_after", &self.forget_after)
      .finish_non_exhaustive()
  }
}

impl Default for LoginThrottle {
  /// The defaults over a fresh [`MemoryThrottleStore`] on the system clock.
  fn default() -> Self {
    Self::new(MemoryThrottleStore::new())
  }
}

fn account_key(account: &str) -> String {
  format!("account:{}", account.to_lowercase())
}

fn client_key(client: IpAddr) -> String {
  format!("ip:{client}")
}

impl LoginThrottle {
  /// A throttle counting in `store`, with the default attempts and delays.
  pub fn new(store: impl ThrottleStore) -> Self {
    Self {
      store: Arc::new(store),
      free_attempts: DEFAULT_FREE_ATTEMPTS,
      base_delay: DEFAULT_BASE_DELAY,
      max_delay: DEFAULT_MAX_DELAY,
      forget_after: DEFAULT_FORGET_AFTER,
      clock: WallClock::system(),
    }
  }

  /// Failures a key may have before it is locked (default [`DEFAULT_FREE_ATTEMPTS`]).
  #[must_use]
  pub fn free_attempts(mut self, attempts: u32) -> Self {
    self.free_attempts = attempts;
    self
  }

  /// The lock after the first failure past the free ones (default [`DEFAULT_BASE_DELAY`]); it
  /// doubles with each further failure.
  #[must_use]
  pub fn base_delay(mut self, delay: Duration) -> Self {
    self.base_delay = delay;
    self
  }

  /// The longest lock (default [`DEFAULT_MAX_DELAY`]).
  #[must_use]
  pub fn max_delay(mut self, delay: Duration) -> Self {
    self.max_delay = delay;
    self
  }

  /// Forget a key's failures once this long passes without a new one (default
  /// [`DEFAULT_FORGET_AFTER`]).
  #[must_use]
  pub fn forget_after(mut self, after: Duration) -> Self {
    self.forget_after = after;
    self
  }

  /// Read the time from `clock` (an anchored clock follows a `TestExecutor`'s virtual time).
  #[must_use]
  pub fn with_clock(mut self, clock: WallClock) -> Self {
    self.clock = clock;
    self
  }

  /// How long a key is locked after `failures` failures: zero up to the free attempts, then
  /// [`base_delay`](Self::base_delay) doubling per failure, at most
  /// [`max_delay`](Self::max_delay).
  pub fn delay_after(&self, failures: u32) -> Duration {
    let Some(past) = failures
      .checked_sub(self.free_attempts)
      .filter(|_| failures > 0)
    else {
      return Duration::ZERO;
    };
    let factor = 1_u32.checked_shl(past).unwrap_or(u32::MAX);
    self.base_delay.saturating_mul(factor).min(self.max_delay)
  }

  /// How much of the lock of `failures` is left at `now`.
  fn remaining(&self, failures: Option<Failures>, now: UtcDateTime) -> Duration {
    let Some(f) = failures else {
      return Duration::ZERO;
    };
    let since = elapsed(f.last, now);
    if since > self.forget_after {
      return Duration::ZERO;
    }
    self.delay_after(f.count).saturating_sub(since)
  }

  /// Whether `account` may try to log in now from `client`: fails with
  /// [`AuthError::TooManyAttempts`] while the account or the address is locked (or
  /// [`AuthError::Unavailable`] when the store fails). Call it before looking the user up or
  /// hashing.
  pub fn check(&self, account: &str, client: Option<IpAddr>) -> Effect<(), AuthError> {
    let this = self.clone();
    let store = Arc::clone(&self.store);
    let account = account_key(account);
    self.clock.now().flat_map(move |now| {
      store.get(&account).flat_map(move |by_account| {
        let by_client = match client {
          Some(ip) => this.store.get(&client_key(ip)),
          None => succeed(None),
        };
        by_client.flat_map(move |by_client| {
          let wait = this
            .remaining(by_account, now)
            .max(this.remaining(by_client, now));
          if wait.is_zero() {
            succeed(())
          } else {
            fail(AuthError::TooManyAttempts { retry_after: wait })
          }
        })
      })
    })
  }

  /// Count a failed attempt against `account` and `client`.
  pub fn record_failure(&self, account: &str, client: Option<IpAddr>) -> Effect<(), AuthError> {
    let this = self.clone();
    let account = account_key(account);
    self.clock.now().flat_map(move |now| {
      let forget = this.forget_after;
      let store = Arc::clone(&this.store);
      this
        .store
        .record_failure(&account, now, forget)
        .flat_map(move |_| match client {
          Some(ip) => store
            .record_failure(&client_key(ip), now, forget)
            .map(|_| ()),
          None => succeed(()),
        })
    })
  }

  /// Clear `account`'s failures after a successful login (the address's count stays; see the
  /// [module documentation](self)).
  pub fn record_success(&self, account: &str) -> Effect<(), AuthError> {
    self.store.clear(&account_key(account))
  }
}

#[cfg(test)]
mod tests {
  use std::net::Ipv4Addr;
  use std::time::Instant;

  use id_effect::{Cause, Exit};

  use super::*;

  fn run<A: Send + 'static>(eff: Effect<A, AuthError>) -> Result<A, AuthError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  fn at(secs: i64) -> UtcDateTime {
    UtcDateTime::unsafe_make(secs * 1000)
  }

  #[test]
  fn delays_double_from_the_base_up_to_the_cap() {
    let t = LoginThrottle::default();
    assert_eq!(t.delay_after(0), Duration::ZERO);
    assert_eq!(t.delay_after(4), Duration::ZERO);
    assert_eq!(t.delay_after(5), Duration::from_secs(1));
    assert_eq!(t.delay_after(6), Duration::from_secs(2));
    assert_eq!(t.delay_after(10), Duration::from_secs(32));
    assert_eq!(t.delay_after(15), DEFAULT_MAX_DELAY);
    assert_eq!(t.delay_after(u32::MAX), DEFAULT_MAX_DELAY);
    assert_eq!(
      LoginThrottle::default().free_attempts(0).delay_after(0),
      Duration::ZERO
    );
    assert!(format!("{t:?}").contains("free_attempts: 5"));
  }

  #[test]
  fn the_memory_store_counts_forgets_and_prunes() {
    let s = MemoryThrottleStore::new();
    let hour = Duration::from_secs(3600);
    assert_eq!(run(s.get("k")), Ok(None));
    assert_eq!(run(s.record_failure("k", at(0), hour)).unwrap().count, 1);
    assert_eq!(run(s.record_failure("k", at(10), hour)).unwrap().count, 2);
    assert_eq!(
      run(s.record_failure("k", at(10 + 3601), hour))
        .unwrap()
        .count,
      1,
      "forgotten after an hour without a failure"
    );
    run(s.clear("k")).unwrap();
    assert_eq!(run(s.get("k")), Ok(None));
    for i in 0..MEMORY_PRUNE_AT {
      run(s.record_failure(&format!("old{i}"), at(0), hour)).unwrap();
    }
    run(s.record_failure("new", at(7200), hour)).unwrap();
    assert_eq!(s.state().len(), 1, "forgotten keys were pruned");
    assert!(format!("{s:?}").contains("keys: 1"));
    assert_eq!(elapsed(at(5), at(1)), Duration::ZERO);
  }

  #[test]
  fn a_lock_runs_from_the_last_failure_and_success_clears_the_account() {
    let clock = WallClock::anchored(Instant::now(), at(1_000));
    let t = LoginThrottle::default()
      .free_attempts(2)
      .base_delay(Duration::from_secs(10))
      .max_delay(Duration::from_secs(25))
      .forget_after(Duration::from_secs(600))
      .with_clock(clock.clone());
    let ip = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
    for _ in 0..2 {
      run(t.check("Alice", ip)).unwrap();
      run(t.record_failure("alice", ip)).unwrap();
    }
    let Err(AuthError::TooManyAttempts { retry_after }) = run(t.check("ALICE", None)) else {
      panic!("the account is locked whatever the case of its name")
    };
    assert!(retry_after <= Duration::from_secs(10) && retry_after > Duration::from_secs(9));
    assert!(
      run(t.check("bob", ip)).is_err(),
      "the address is locked too"
    );
    assert!(run(t.check("bob", None)).is_ok());
    clock.jump(Duration::from_secs(10));
    run(t.check("alice", ip)).unwrap();
    run(t.record_failure("alice", None)).unwrap();
    clock.jump(Duration::from_secs(19));
    assert!(run(t.check("alice", None)).is_err(), "20 s after the third");
    clock.jump(Duration::from_secs(1));
    run(t.check("alice", None)).unwrap();
    run(t.record_success("alice")).unwrap();
    run(t.record_failure("alice", None)).unwrap();
    assert!(
      run(t.check("alice", None)).is_ok(),
      "the count started over"
    );
    clock.jump(Duration::from_secs(601));
    assert!(run(t.check("bob", ip)).is_ok(), "the address was forgotten");
  }
}
