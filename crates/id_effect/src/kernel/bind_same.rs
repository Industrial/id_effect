//! The `SameEnv` wrapper and its `Bindable` impl: the H17 pattern for binding helpers with `~`.
//!
//! `~sub` narrows: it runs `sub: Effect<_, _, Sub>` under any `Sub` the caller's `R` embeds. That
//! is what lets a helper over `caps!(Logger)` be bound from a caller over
//! `caps!(Logger, Db)`. It also means nothing fixes `Sub` when the helper is **generic** in
//! its environment: `~audit(msg)` with `fn audit<R: Selector<Logger, I>, I>` is ambiguous
//! (E0283), and so is an inline `~need::<Db>().map(..)`. Two forms solve it, both with 0
//! annotations at the call site.
//!
//! # (i) Recommended: helpers over a concrete `caps!(..)`
//!
//! Give the helper exactly the capabilities it uses. `~` narrows the caller's environment to it.
//!
//! ```
//! use std::sync::{Arc, Mutex};
//!
//! use id_effect::{Effect, caps, effect, hlist};
//! use id_effect::{Exit, Never};
//!
//! #[derive(Clone, Default)]
//! struct Logger(Arc<Mutex<Vec<String>>>);
//! impl Logger {
//!   fn info(&self, m: String) {
//!     self.0.lock().unwrap().push(m);
//!   }
//! }
//! struct Db(usize);
//!
//! fn audit(msg: String) -> Effect<(), Never, caps!(Logger)> {
//!   effect!({ ~Logger.info(msg) })
//! }
//!
//! fn signup() -> Effect<usize, Never, caps!(Logger, Db)> {
//!   effect!({
//!     let n = ~Db.0;
//!     ~audit(format!("signup: {n}"));
//!     n
//!   })
//! }
//!
//! let logger = Logger::default();
//! let exit = signup().provide_env(hlist![logger.clone(), Db(3)]).block_on();
//! assert!(matches!(exit, Exit::Success(3)));
//! assert_eq!(*logger.0.lock().unwrap(), vec!["signup: 3".to_string()]);
//! ```
//!
//! # (ii) Helpers generic in `R`: `.same_env()`
//!
//! [`Effect::same_env`] wraps the helper's effect in [`SameEnv`], whose [`Bindable`] impl binds
//! it over **exactly** the caller's environment (`Sub = R`, the `Arc` is shared, nothing is
//! projected). That fixes the helper's `R` from the caller, so `~audit(msg).same_env()` needs no
//! turbofish (the prototype wrote `~audit::<SignupCaps, _>(..)`). The same works for an inline
//! `~need::<T>().map(..).same_env()`.
//!
//! ```
//! use std::sync::{Arc, Mutex};
//!
//! use id_effect::{Effect, HList, Selector, caps, effect, hlist, need};
//! use id_effect::{Exit, Never};
//!
//! #[derive(Clone, Default)]
//! struct Logger(Arc<Mutex<Vec<String>>>);
//! impl Logger {
//!   fn info(&self, m: String) {
//!     self.0.lock().unwrap().push(m);
//!   }
//! }
//! struct Db(usize);
//! struct Mailer;
//!
//! fn audit<R, I>(msg: String) -> Effect<(), Never, R>
//! where
//!   R: HList + Selector<Logger, I>,
//! {
//!   effect!({ ~Logger.info(msg) })
//! }
//!
//! fn signup() -> Effect<usize, Never, caps!(Logger, Db)> {
//!   effect!({
//!     let n = ~need::<Db>().map(|db| db.0).same_env();
//!     ~audit(format!("signup: {n}")).same_env();
//!     n
//!   })
//! }
//!
//! fn notify() -> Effect<(), Never, caps!(Mailer, Logger)> {
//!   effect!({ ~audit("notify".to_string()).same_env() })
//! }
//!
//! let logger = Logger::default();
//! let exit = signup().provide_env(hlist![logger.clone(), Db(3)]).block_on();
//! assert!(matches!(exit, Exit::Success(3)));
//! let exit = notify().provide_env(hlist![Mailer, logger.clone()]).block_on();
//! assert!(matches!(exit, Exit::Success(())));
//! assert_eq!(*logger.0.lock().unwrap(), vec!["signup: 3", "notify"]);
//! ```
//!
//! Without `.same_env()` the generic helper does not compile:
//!
//! ```compile_fail
//! use id_effect::Never;
//! use id_effect::{Effect, HList, Selector, caps, effect};
//!
//! struct Logger;
//! struct Db;
//!
//! fn audit<R: HList + Selector<Logger, I>, I>() -> Effect<(), Never, R> {
//!   effect!({ let _ = ~Logger; })
//! }
//!
//! fn signup() -> Effect<(), Never, caps!(Logger, Db)> {
//!   effect!({ ~audit() })
//! }
//! ```
//!
//! **Name.** `same_env` (the plan's default): it says what the bind does (no narrowing) rather
//! than why it is needed.

use super::bind::{BindFuture, Bindable};
use super::ctx::Ctx;
use super::effect::Effect;

/// An effect to be `~`-bound over exactly the enclosing environment (see the
/// [module docs](self)). Built by [`Effect::same_env`].
#[must_use = "a `SameEnv` does nothing unless bound with `~` / `Ctx::bind`"]
#[derive(Debug)]
pub struct SameEnv<X>(X);

impl<X> SameEnv<X> {
  /// The wrapped effect.
  pub fn into_inner(self) -> X {
    self.0
  }
}

/// The [`Bindable`] index of a [`SameEnv`] bind (no projection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SameEnvIndex;

impl<A, E, R> Effect<A, E, R> {
  /// Marks this effect to be `~`-bound over exactly the caller's environment, which fixes a
  /// generic `R` from the caller (H17; see `kernel/bind_same.rs`).
  pub fn same_env(self) -> SameEnv<Self> {
    SameEnv(self)
  }
}

impl<B, E2, R> Bindable<R, SameEnvIndex> for SameEnv<Effect<B, E2, R>>
where
  B: Send + 'static,
  E2: Send + 'static,
  R: Send + Sync + 'static,
{
  type Output = B;
  type Error = E2;

  fn bind_in(self, ctx: &Ctx<R>) -> BindFuture<B, E2> {
    ctx.bind_same_env(self.0)
  }
}
