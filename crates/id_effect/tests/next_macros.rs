//! `k-macros-next` acceptance tests: `effect_next!`, `effect_local_next!`, `caps_next!`, the H17
//! generic-helper pattern (`.same_env()`).
//!
//! Effects run on `Runtime::global()` through `Effect::block_on` / `LocalEffect::block_on`.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use id_effect::next::{
  Effect, HCons, HList, HNil, LocalEffect, Selector, caps_next, effect_local_next, effect_next,
  fail, hlist, need, succeed,
};
use id_effect::{Cause, Exit, Never};

// ── capabilities ────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Greeting(&'static str);

#[derive(Debug)]
struct Counter(usize);

#[derive(Clone, Debug, Default)]
struct Logger(Arc<Mutex<Vec<String>>>);

impl Logger {
  fn info(&self, m: impl Into<String>) {
    self
      .0
      .lock()
      .unwrap_or_else(|p| p.into_inner())
      .push(m.into());
  }
  fn lines(&self) -> Vec<String> {
    self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
  }
}

#[derive(Debug)]
struct Db {
  users: usize,
  failing: bool,
}

impl Db {
  fn with_users(users: usize) -> Self {
    Self {
      users,
      failing: false,
    }
  }
  fn count(&self, _table: &str) -> Result<usize, DbError> {
    if self.failing {
      Err(DbError("unavailable"))
    } else {
      Ok(self.users)
    }
  }
}

#[derive(Clone, Debug, Default)]
struct Mailer(Arc<Mutex<Vec<String>>>);

impl Mailer {
  fn send(&self, to: &str) -> Result<(), AppError> {
    self
      .0
      .lock()
      .unwrap_or_else(|p| p.into_inner())
      .push(to.to_string());
    Ok(())
  }
  fn sent(&self) -> usize {
    self.0.lock().unwrap_or_else(|p| p.into_inner()).len()
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DbError(&'static str);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AppError {
  Db(DbError),
}

impl From<DbError> for AppError {
  fn from(e: DbError) -> Self {
    Self::Db(e)
  }
}

fn same_type<T>(_: PhantomData<T>, _: PhantomData<T>) {}

// ── caps_next! ──────────────────────────────────────────────────────────────────────────────

#[test]
fn caps_next_expands_to_a_right_nested_hlist() {
  same_type(
    PhantomData::<caps_next!(Greeting, Counter, Logger)>,
    PhantomData::<HCons<Greeting, HCons<Counter, HCons<Logger, HNil>>>>,
  );
  same_type(PhantomData::<caps_next!()>, PhantomData::<HNil>);
  // Generic arguments with commas stay one capability.
  same_type(
    PhantomData::<caps_next!(Vec<(u8, u16)>, Counter,)>,
    PhantomData::<HCons<Vec<(u8, u16)>, HCons<Counter, HNil>>>,
  );
}

// ── effect_next! forms ──────────────────────────────────────────────────────────────────────

fn answer() -> Effect<u32> {
  succeed(42)
}

fn hello() -> Effect<String, Never, caps_next!(Greeting)> {
  effect_next!({ ~Greeting.0.to_string() })
}

fn count_users() -> Effect<usize, DbError, caps_next!(Db)> {
  effect_next!({ ~Db.count("users")? })
}

#[test]
fn cap_read_and_never_widening_bind() {
  // `~answer()` is `Never`-typed and binds under a `DbError` body.
  let eff: Effect<String, DbError, caps_next!(Greeting)> = effect_next!({
    let n = ~answer();
    format!("{}:{n}", ~Greeting.0)
  });
  assert_eq!(
    eff.provide(Greeting("hi")).block_on(),
    Exit::Success("hi:42".to_string())
  );
}

#[test]
fn narrowing_bind_with_from_widening_and_typed_failure() {
  type Caps = caps_next!(Greeting, Db);
  fn summary() -> Effect<String, AppError, Caps> {
    effect_next!({
      let g = ~hello();
      let n = ~count_users();
      format!("{g}:{n}")
    })
  }
  let ok = summary()
    .provide_env(hlist![Greeting("hi"), Db::with_users(3)])
    .block_on();
  assert_eq!(ok, Exit::Success("hi:3".to_string()));
  let failing = Db {
    users: 0,
    failing: true,
  };
  let err = summary()
    .provide_env(hlist![Greeting("hi"), failing])
    .block_on();
  assert_eq!(
    err,
    Exit::Failure(Cause::Fail(AppError::Db(DbError("unavailable"))))
  );
}

#[test]
fn head_forms_bind_the_environment() {
  let typed: Effect<String, Never, _> = effect_next!(|r: &caps_next!(Greeting, Counter)| {
    format!("{}/{}", r.0.0, (r.1).0.0 + ~Counter.0)
  });
  let exit = typed
    .provide_env(hlist![Greeting("g"), Counter(2)])
    .block_on();
  assert!(matches!(exit, Exit::Success(ref s) if s == "g/4"));

  let empty: Effect<u8> = effect_next!(|| { 7 });
  assert!(matches!(empty.block_on(), Exit::Success(7)));

  let bare: Effect<u8> = effect_next!({ 1 + 1 });
  assert!(matches!(bare.block_on(), Exit::Success(2)));
}

#[test]
fn nested_effect_next_keeps_its_own_binds() {
  let outer: Effect<usize, Never, caps_next!(Greeting, Counter)> = effect_next!({
    let inner: Effect<usize, Never, caps_next!(Greeting)> =
      effect_next!({ ~Greeting.0.len() });
    ~inner + ~Counter.0
  });
  assert!(matches!(
    outer
      .provide_env(hlist![Greeting("abc"), Counter(1)])
      .block_on(),
    Exit::Success(4)
  ));
}

#[test]
fn typed_failure_of_a_bound_effect_short_circuits() {
  let ran = Arc::new(Mutex::new(false));
  let ran2 = Arc::clone(&ran);
  let eff: Effect<(), DbError> = effect_next!({
    ~fail::<(), DbError, HNil>(DbError("boom"));
    *ran2.lock().unwrap_or_else(|p| p.into_inner()) = true;
  });
  assert_eq!(eff.block_on(), Exit::Failure(Cause::Fail(DbError("boom"))));
  assert!(!*ran.lock().unwrap_or_else(|p| p.into_inner()));
}

// ── effect_local_next! ──────────────────────────────────────────────────────────────────────

fn local_part() -> LocalEffect<usize, DbError, caps_next!(Counter)> {
  effect_local_next!({
    let held = Rc::new(5_usize);
    ~LocalEffect::<(), Never, HNil>::yield_fiber();
    *held + ~Counter.0
  })
}

type LocalCaps = caps_next!(Greeting, Counter, Db);

#[test]
fn effect_local_next_builds_a_local_effect_holding_rc_across_awaits() {
  let eff: LocalEffect<String, AppError, LocalCaps> = effect_local_next!({
    let acc = Rc::new(RefCell::new(Vec::<String>::new()));
    let g = ~hello(); // `Send` effect, narrowed
    acc.borrow_mut().push(g);
    ~LocalEffect::<(), Never, HNil>::yield_fiber();
    let n = ~local_part(); // `LocalEffect`, narrowed, `DbError` → `AppError`
    acc.borrow_mut().push(n.to_string());
    let users = ~count_users();
    acc.borrow_mut().push(users.to_string());
    acc.borrow().join(",")
  });
  let exit = eff
    .provide_env(hlist![Greeting("hi"), Counter(1), Db::with_users(9)])
    .block_on();
  assert_eq!(exit, Exit::Success("hi,6,9".to_string()));
}

#[test]
fn effect_local_next_surfaces_typed_failures() {
  let eff: LocalEffect<usize, AppError, caps_next!(Db)> = effect_local_next!({ ~count_users() });
  let failing = Db {
    users: 0,
    failing: true,
  };
  assert_eq!(
    eff.provide(failing).block_on(),
    Exit::Failure(Cause::Fail(AppError::Db(DbError("unavailable"))))
  );
}

// ── H17: one generic helper bound from two environments, 0 turbofish ───────────────────────

fn audit<R, I>(msg: String) -> Effect<(), AppError, R>
where
  R: HList + Selector<Logger, I>,
{
  effect_next!({ ~Logger.info(msg) })
}

type SignupCaps = caps_next!(Logger, Db);
type NotifyCaps = caps_next!(Logger, Mailer);

fn signup() -> Effect<usize, AppError, SignupCaps> {
  effect_next!({
    let n = ~count_users();
    ~audit(format!("signup: {n} users")).same_env();
    n
  })
}

fn notify() -> Effect<(), AppError, NotifyCaps> {
  effect_next!({
    ~Mailer.send("ops@example.com")?;
    ~audit("notify: sent".to_string()).same_env();
  })
}

#[test]
fn h17_one_generic_helper_from_two_envs() {
  let logger = Logger::default();
  let n = signup()
    .provide_env(hlist![logger.clone(), Db::with_users(3)])
    .block_on();
  assert_eq!(n, Exit::Success(3));
  let mailer = Mailer::default();
  let sent = notify()
    .provide_env(hlist![logger.clone(), mailer.clone()])
    .block_on();
  assert_eq!(sent, Exit::Success(()));
  assert_eq!(logger.lines(), vec!["signup: 3 users", "notify: sent"]);
  assert_eq!(mailer.sent(), 1);
}

#[test]
fn h17_inline_need_binds_with_same_env() {
  let eff: Effect<usize, Never, caps_next!(Counter, Greeting)> = effect_next!({
    ~need::<Counter>().map(|c| c.0 + 1).same_env()
  });
  assert!(matches!(
    eff
      .provide_env(hlist![Counter(1), Greeting("g")])
      .block_on(),
    Exit::Success(2)
  ));
}
