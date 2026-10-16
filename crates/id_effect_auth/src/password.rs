//! Password hashing: argon2id PHC strings, verification that reports when a stored hash should
//! be upgraded, and a dummy verification for unknown users.
//!
//! [`PasswordHasher`] is the capability (environment [`PasswordHasherEnv`], layer
//! [`password_hasher_layer`]). Its defaults are the OWASP Password Storage Cheat Sheet's argon2id
//! parameters ([`PasswordParams::OWASP`]). Argon2 is deliberately slow, so every hash and verify
//! runs as one job on the executor's blocking pool (`Executor::spawn_blocking`), never on a fiber
//! thread.
//!
//! ```
//! use id_effect::Exit;
//! use id_effect_auth::password::{PasswordHasher, PasswordParams, Verification};
//!
//! // Low cost to keep the doctest fast; production code uses `PasswordHasher::default()`.
//! let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap());
//! let program = hasher
//!   .hash("correct horse")
//!   .flat_map(move |phc| hasher.verify("correct horse", &phc));
//! assert!(matches!(program.block_on(), Exit::Success(Verification::Valid)));
//! ```

use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use argon2::password_hash::phc::PasswordHash as Phc;
use argon2::{
  ARGON2ID_IDENT, Algorithm, Argon2, Params, PasswordHasher as _, PasswordVerifier as _, Version,
};
use id_effect::{Ctx, Effect, HCons, HList, HNil, Layer, Selector, hlist, need};

/// Argon2id cost parameters: memory in KiB, iterations and lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PasswordParams {
  m_cost_kib: u32,
  t_cost: u32,
  p_cost: u32,
}

impl PasswordParams {
  /// The OWASP Password Storage Cheat Sheet's argon2id minimum: 19 MiB of memory, 2 iterations,
  /// 1 lane. [`PasswordHasher::default`] uses it.
  pub const OWASP: Self = Self {
    m_cost_kib: 19 * 1024,
    t_cost: 2,
    p_cost: 1,
  };

  /// Parameters with `m_cost_kib` KiB of memory, `t_cost` iterations and `p_cost` lanes.
  ///
  /// # Errors
  ///
  /// [`PasswordError::Params`] when argon2 rejects them (memory below 8 KiB per lane, zero
  /// iterations or lanes).
  pub fn new(m_cost_kib: u32, t_cost: u32, p_cost: u32) -> Result<Self, PasswordError> {
    Params::new(m_cost_kib, t_cost, p_cost, None)
      .map_err(|e| PasswordError::Params(e.to_string()))?;
    Ok(Self {
      m_cost_kib,
      t_cost,
      p_cost,
    })
  }

  /// Memory cost in KiB.
  #[inline]
  pub const fn m_cost_kib(self) -> u32 {
    self.m_cost_kib
  }

  /// Iterations.
  #[inline]
  pub const fn t_cost(self) -> u32 {
    self.t_cost
  }

  /// Lanes (degree of parallelism).
  #[inline]
  pub const fn p_cost(self) -> u32 {
    self.p_cost
  }

  fn argon2(self) -> Argon2<'static> {
    // Validated by `new` (or a constant that is valid), so `DEFAULT` is never reached.
    let params =
      Params::new(self.m_cost_kib, self.t_cost, self.p_cost, None).unwrap_or(Params::DEFAULT);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
  }
}

impl Default for PasswordParams {
  fn default() -> Self {
    Self::OWASP
  }
}

/// A plaintext password. `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
  /// Wrap `plaintext`.
  #[inline]
  pub fn new(plaintext: impl Into<String>) -> Self {
    Self(plaintext.into())
  }

  /// The plaintext, for the hashing call only.
  #[inline]
  pub fn expose(&self) -> &str {
    &self.0
  }
}

impl fmt::Debug for Password {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("Password(<redacted>)")
  }
}

impl From<String> for Password {
  fn from(s: String) -> Self {
    Self(s)
  }
}

impl From<&str> for Password {
  fn from(s: &str) -> Self {
    Self(s.to_owned())
  }
}

/// A stored password hash: a PHC string such as `$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PasswordHash(String);

impl PasswordHash {
  /// Parse and keep a PHC string read from storage.
  ///
  /// # Errors
  ///
  /// [`PasswordError::MalformedHash`] when `phc` is not a PHC string.
  pub fn parse(phc: impl Into<String>) -> Result<Self, PasswordError> {
    let phc = phc.into();
    Phc::new(&phc).map_err(|e| PasswordError::MalformedHash(e.to_string()))?;
    Ok(Self(phc))
  }

  /// The PHC string, to store.
  #[inline]
  pub fn as_str(&self) -> &str {
    &self.0
  }

  fn phc(&self) -> Result<Phc, PasswordError> {
    Phc::new(&self.0).map_err(|e| PasswordError::MalformedHash(e.to_string()))
  }
}

impl fmt::Debug for PasswordHash {
  /// Shows the algorithm and parameters, not the salt and digest.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let shown: Vec<&str> = self.0.split('$').take(4).collect();
    write!(f, "PasswordHash({}$…)", shown.join("$"))
  }
}

impl fmt::Display for PasswordHash {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

impl FromStr for PasswordHash {
  type Err = PasswordError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::parse(s)
  }
}

/// The outcome of a verification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verification {
  /// Wrong password (or unknown user).
  Invalid,
  /// Correct password; the stored hash uses the current parameters.
  Valid,
  /// Correct password; the stored hash used other parameters or another algorithm, so here is a
  /// hash of the same password under the current ones. Store it.
  ValidNeedsRehash(PasswordHash),
}

impl Verification {
  /// `true` for [`Valid`](Self::Valid) and [`ValidNeedsRehash`](Self::ValidNeedsRehash).
  #[inline]
  pub fn is_valid(&self) -> bool {
    !matches!(self, Self::Invalid)
  }
}

/// Password hashing failures. A wrong password is not an error: it is
/// [`Verification::Invalid`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PasswordError {
  /// Argon2 rejected the cost parameters.
  Params(String),
  /// A stored hash is not a PHC string.
  MalformedHash(String),
  /// Hashing failed (salt generation, output size).
  Hash(String),
  /// The blocking-pool job ended without a result (it panicked).
  Blocking,
}

impl fmt::Display for PasswordError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Params(m) => write!(f, "invalid argon2 parameters: {m}"),
      Self::MalformedHash(m) => write!(f, "malformed password hash: {m}"),
      Self::Hash(m) => write!(f, "password hashing failed: {m}"),
      Self::Blocking => f.write_str("password hashing job ended without a result"),
    }
  }
}

impl std::error::Error for PasswordError {}

/// Runs `job` on the running fiber's blocking pool and resumes the fiber with its result.
///
/// The fiber suspends (no thread is held) until the job finishes. Interrupting the fiber does not
/// stop a job that already started; its result is dropped.
pub(crate) fn on_blocking_pool<A, E>(
  job: impl FnOnce() -> Result<A, E> + Send + 'static,
  lost: impl FnOnce() -> E + Send + 'static,
) -> Effect<A, E>
where
  A: Send + 'static,
  E: Send + 'static,
{
  Effect::from_ctx(move |ctx: Ctx<HNil>| {
    let (tx, rx) = flume::bounded(1);
    ctx.fiber().executor().spawn_blocking(Box::new(move || {
      // The receiver is gone only when the fiber was interrupted meanwhile.
      let _ = tx.send(job());
    }));
    async move { rx.recv_async().await.unwrap_or_else(|_| Err(lost())) }
  })
}

/// Capability: argon2id password hashing (cheap to clone; clones share the dummy hash).
#[derive(Clone)]
pub struct PasswordHasher {
  params: PasswordParams,
  argon2: Argon2<'static>,
  /// A hash under `params` that no password verifies against (built on first use).
  dummy: Arc<OnceLock<PasswordHash>>,
}

impl fmt::Debug for PasswordHasher {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("PasswordHasher")
      .field("params", &self.params)
      .finish_non_exhaustive()
  }
}

impl Default for PasswordHasher {
  /// argon2id with [`PasswordParams::OWASP`].
  fn default() -> Self {
    Self::new(PasswordParams::OWASP)
  }
}

impl PasswordHasher {
  /// A hasher producing argon2id hashes with `params`.
  pub fn new(params: PasswordParams) -> Self {
    Self {
      params,
      argon2: params.argon2(),
      dummy: Arc::new(OnceLock::new()),
    }
  }

  /// The parameters new hashes use.
  #[inline]
  pub fn params(&self) -> PasswordParams {
    self.params
  }

  /// Hash `password` with a fresh random salt (on the blocking pool).
  pub fn hash(&self, password: impl Into<Password>) -> Effect<PasswordHash, PasswordError> {
    let (argon2, password) = (self.argon2.clone(), password.into());
    on_blocking_pool(
      move || hash_now(&argon2, &password),
      || PasswordError::Blocking,
    )
  }

  /// Check `password` against `stored` (on the blocking pool). The comparison is constant-time;
  /// a correct password whose hash used other parameters yields
  /// [`Verification::ValidNeedsRehash`] with a hash under the current ones.
  pub fn verify(
    &self,
    password: impl Into<Password>,
    stored: &PasswordHash,
  ) -> Effect<Verification, PasswordError> {
    let (hasher, password, stored) = (self.clone(), password.into(), stored.clone());
    on_blocking_pool(
      move || hasher.verify_now(&password, &stored),
      || PasswordError::Blocking,
    )
  }

  /// [`verify`](Self::verify) for a login whose user may not exist: with `stored = None` the
  /// password is checked against a dummy hash with the same parameters, so an unknown user costs
  /// the same time as a wrong password and the response time does not reveal which accounts
  /// exist. The result is then always [`Verification::Invalid`].
  pub fn verify_or_dummy(
    &self,
    password: impl Into<Password>,
    stored: Option<&PasswordHash>,
  ) -> Effect<Verification, PasswordError> {
    let (hasher, password, stored) = (self.clone(), password.into(), stored.cloned());
    on_blocking_pool(
      move || match stored {
        Some(stored) => hasher.verify_now(&password, &stored),
        None => {
          let dummy = hasher.dummy_hash()?;
          hasher.verify_now(&password, &dummy)?;
          Ok(Verification::Invalid)
        }
      },
      || PasswordError::Blocking,
    )
  }

  /// `true` when `stored` is not argon2id v19 under this hasher's parameters.
  ///
  /// # Errors
  ///
  /// [`PasswordError::MalformedHash`] when `stored` cannot be parsed.
  pub fn needs_rehash(&self, stored: &PasswordHash) -> Result<bool, PasswordError> {
    let phc = stored.phc()?;
    let current = phc.algorithm == ARGON2ID_IDENT
      && phc.version == Some(Version::V0x13.into())
      && Params::try_from(&phc).is_ok_and(|p| {
        (p.m_cost(), p.t_cost(), p.p_cost())
          == (
            self.params.m_cost_kib,
            self.params.t_cost,
            self.params.p_cost,
          )
      });
    Ok(!current)
  }

  /// The dummy hash (computed once per hasher, then shared by its clones).
  pub(crate) fn dummy_hash(&self) -> Result<PasswordHash, PasswordError> {
    if let Some(h) = self.dummy.get() {
      return Ok(h.clone());
    }
    let h = hash_now(
      &self.argon2,
      &Password::new("id_effect_auth dummy password"),
    )?;
    Ok(self.dummy.get_or_init(|| h).clone())
  }

  fn verify_now(
    &self,
    password: &Password,
    stored: &PasswordHash,
  ) -> Result<Verification, PasswordError> {
    let phc = stored.phc()?;
    if self
      .argon2
      .verify_password(password.expose().as_bytes(), &phc)
      .is_err()
    {
      return Ok(Verification::Invalid);
    }
    if self.needs_rehash(stored)? {
      return hash_now(&self.argon2, password).map(Verification::ValidNeedsRehash);
    }
    Ok(Verification::Valid)
  }
}

fn hash_now(argon2: &Argon2<'static>, password: &Password) -> Result<PasswordHash, PasswordError> {
  argon2
    .hash_password(password.expose().as_bytes())
    .map(|h| PasswordHash(h.to_string()))
    .map_err(|e| PasswordError::Hash(e.to_string()))
}

/// The minimal typed environment holding only [`PasswordHasher`].
pub type PasswordHasherEnv = HCons<PasswordHasher, HNil>;

/// A [`Layer`] providing `hasher`.
#[inline]
pub fn password_hasher_layer<E: Send + 'static>(
  hasher: PasswordHasher,
) -> Layer<PasswordHasherEnv, E> {
  Layer::succeed(hlist![hasher])
}

/// [`PasswordHasher::hash`] on the hasher in `R`.
pub fn hash_password<R, I>(password: impl Into<Password>) -> Effect<PasswordHash, PasswordError, R>
where
  R: Selector<PasswordHasher, I> + HList,
{
  let password = password.into();
  need::<PasswordHasher>().flat_map(move |h| h.hash(password).narrow())
}

/// [`PasswordHasher::verify_or_dummy`] on the hasher in `R`.
pub fn verify_password<R, I>(
  password: impl Into<Password>,
  stored: Option<PasswordHash>,
) -> Effect<Verification, PasswordError, R>
where
  R: Selector<PasswordHasher, I> + HList,
{
  let password = password.into();
  need::<PasswordHasher>().flat_map(move |h| h.verify_or_dummy(password, stored.as_ref()).narrow())
}

#[cfg(test)]
mod tests {
  use std::time::{Duration, Instant};

  use id_effect::{Cause, Exit, HCons, HNil};

  use super::*;

  /// Low-cost parameters: tests stay fast; production uses `PasswordParams::OWASP`.
  fn fast() -> PasswordParams {
    PasswordParams::new(8, 1, 1).expect("valid test params")
  }

  fn run<A: Send + 'static>(eff: Effect<A, PasswordError>) -> Result<A, PasswordError> {
    match eff.block_on() {
      Exit::Success(a) => Ok(a),
      Exit::Failure(Cause::Fail(e)) => Err(e),
      Exit::Failure(c) => panic!("unexpected cause: {c:?}"),
    }
  }

  #[test]
  fn defaults_are_owasp_argon2id() {
    let h = PasswordHasher::default();
    assert_eq!(h.params(), PasswordParams::OWASP);
    assert_eq!(PasswordParams::default(), PasswordParams::OWASP);
    assert_eq!(
      (
        PasswordParams::OWASP.m_cost_kib(),
        PasswordParams::OWASP.t_cost(),
        PasswordParams::OWASP.p_cost()
      ),
      (19_456, 2, 1)
    );
    assert!(format!("{h:?}").contains("19456"));
  }

  #[test]
  fn invalid_params_are_rejected() {
    assert!(matches!(
      PasswordParams::new(1, 1, 1),
      Err(PasswordError::Params(_))
    ));
    assert!(matches!(
      PasswordParams::new(64, 0, 1),
      Err(PasswordError::Params(_))
    ));
  }

  #[test]
  fn hash_is_a_salted_argon2id_phc_string() {
    let h = PasswordHasher::new(fast());
    let a = run(h.hash("pw")).unwrap();
    let b = run(h.hash("pw")).unwrap();
    assert!(a.as_str().starts_with("$argon2id$v=19$m=8,t=1,p=1$"), "{a}");
    assert_ne!(a, b, "a fresh salt per hash");
    assert!(!h.needs_rehash(&a).unwrap());
  }

  #[test]
  fn verify_accepts_the_password_and_rejects_others() {
    let h = PasswordHasher::new(fast());
    let stored = run(h.hash("right")).unwrap();
    assert_eq!(
      run(h.verify("right", &stored)).unwrap(),
      Verification::Valid
    );
    assert_eq!(
      run(h.verify("wrong", &stored)).unwrap(),
      Verification::Invalid
    );
    assert!(Verification::Valid.is_valid());
    assert!(!Verification::Invalid.is_valid());
  }

  #[test]
  fn rehash_on_verify_when_params_change() {
    let old = PasswordHasher::new(fast());
    let stored = run(old.hash("pw")).unwrap();
    let new = PasswordHasher::new(PasswordParams::new(16, 1, 1).unwrap());
    assert!(new.needs_rehash(&stored).unwrap());
    let Verification::ValidNeedsRehash(upgraded) = run(new.verify("pw", &stored)).unwrap() else {
      panic!("expected a rehash");
    };
    assert!(upgraded.as_str().contains("m=16,t=1,p=1"), "{upgraded}");
    assert!(!new.needs_rehash(&upgraded).unwrap());
    // A wrong password never yields a rehash.
    assert_eq!(
      run(new.verify("nope", &stored)).unwrap(),
      Verification::Invalid
    );
  }

  #[test]
  fn other_algorithms_need_a_rehash() {
    let h = PasswordHasher::new(fast());
    let argon2i = Argon2::new(
      Algorithm::Argon2i,
      Version::V0x13,
      Params::new(8, 1, 1, None).unwrap(),
    );
    let stored = hash_now(&argon2i, &Password::new("pw")).unwrap();
    assert!(stored.as_str().starts_with("$argon2i$"));
    assert!(h.needs_rehash(&stored).unwrap());
    assert!(matches!(
      run(h.verify("pw", &stored)).unwrap(),
      Verification::ValidNeedsRehash(_)
    ));
  }

  #[test]
  fn malformed_hashes_are_errors() {
    assert!(matches!(
      PasswordHash::parse("not a phc string"),
      Err(PasswordError::MalformedHash(_))
    ));
    let h = PasswordHasher::new(fast());
    let bogus = PasswordHash("no-leading-dollar".to_owned());
    assert!(matches!(
      run(h.verify("pw", &bogus)),
      Err(PasswordError::MalformedHash(_))
    ));
    assert!(h.needs_rehash(&bogus).is_err());
  }

  #[test]
  fn hashes_round_trip_through_strings() {
    let h = PasswordHasher::new(fast());
    let stored = run(h.hash("pw")).unwrap();
    let parsed: PasswordHash = stored.to_string().parse().unwrap();
    assert_eq!(parsed, stored);
    let shown = format!("{stored:?}");
    assert!(
      shown.starts_with("PasswordHash($argon2id$v=19$m=8"),
      "{shown}"
    );
    let digest = stored.as_str().rsplit('$').next().unwrap();
    assert!(!shown.contains(digest), "Debug hides the digest");
  }

  #[test]
  fn passwords_are_redacted() {
    let p = Password::from(String::from("hunter2"));
    assert_eq!(format!("{p:?}"), "Password(<redacted>)");
    assert_eq!(p.expose(), "hunter2");
    assert_eq!(Password::from("x"), Password::new("x"));
  }

  #[test]
  fn unknown_user_verifies_against_a_dummy_with_the_same_params() {
    let h = PasswordHasher::new(fast());
    assert_eq!(
      run(h.verify_or_dummy("pw", None)).unwrap(),
      Verification::Invalid
    );
    let dummy = h.dummy_hash().unwrap();
    assert!(
      !h.needs_rehash(&dummy).unwrap(),
      "same params as real hashes"
    );
    // Computed once and shared by clones.
    assert_eq!(h.clone().dummy_hash().unwrap(), dummy);
    let stored = run(h.hash("pw")).unwrap();
    assert_eq!(
      run(h.verify_or_dummy("pw", Some(&stored))).unwrap(),
      Verification::Valid
    );
  }

  /// Median wall time of `n` runs of `eff()`.
  fn median(n: usize, mut eff: impl FnMut() -> Effect<Verification, PasswordError>) -> Duration {
    let mut times: Vec<Duration> = (0..n)
      .map(|_| {
        let t = Instant::now();
        run(eff()).unwrap();
        t.elapsed()
      })
      .collect();
    times.sort();
    times[n / 2]
  }

  #[test]
  fn verify_is_constant_time_on_unknown_user() {
    // Enough memory that argon2 dominates the measurement (still a few ms).
    let h = PasswordHasher::new(PasswordParams::new(4096, 1, 1).unwrap());
    let stored = run(h.hash("right")).unwrap();
    // Warm the dummy so the first unknown-user call does not pay for building it.
    run(h.verify_or_dummy("x", None)).unwrap();
    let known = median(9, || h.verify_or_dummy("wrong", Some(&stored)));
    let unknown = median(9, || h.verify_or_dummy("wrong", None));
    let ratio = unknown.as_secs_f64() / known.as_secs_f64();
    assert!(
      (0.33..3.0).contains(&ratio),
      "unknown user {unknown:?} vs wrong password {known:?}"
    );
  }

  #[test]
  fn capability_layer_and_helpers() {
    type Env = HCons<PasswordHasher, HNil>;
    let program: Effect<Verification, PasswordError, Env> = hash_password::<Env, _>("pw")
      .flat_map(|stored| verify_password::<Env, _>("pw", Some(stored)));
    let exit = program
      .provide_layer(password_hasher_layer(PasswordHasher::new(fast())))
      .block_on();
    assert!(matches!(exit, Exit::Success(Verification::Valid)));
  }

  #[test]
  fn a_panicking_job_is_a_blocking_error() {
    let eff = on_blocking_pool::<(), PasswordError>(|| panic!("boom"), || PasswordError::Blocking);
    assert_eq!(run(eff), Err(PasswordError::Blocking));
  }

  #[test]
  fn errors_display() {
    for (e, needle) in [
      (PasswordError::Params("m".into()), "parameters"),
      (PasswordError::MalformedHash("x".into()), "malformed"),
      (PasswordError::Hash("salt".into()), "hashing failed"),
      (PasswordError::Blocking, "without a result"),
    ] {
      assert!(e.to_string().contains(needle), "{e}");
    }
  }
}
