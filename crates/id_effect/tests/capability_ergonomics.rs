//! Capability ergonomics (0.6): `caps!` aliases, environment-free steps under `~`, `widen_error`,
//! `#[provides(…, with = …)]`, and call-site context in missing-capability panics.

use id_effect::{
  Effect, Env, FromEnv, Never, ProviderSpecDerive, caps, effect, provide, run_blocking, run_with,
  succeed,
};

#[derive(Clone, Debug, PartialEq)]
struct Greeting(&'static str);

#[derive(Clone, Debug, PartialEq)]
struct Audit(u32);

#[derive(ProviderSpecDerive)]
#[provides(Greeting, with = || Greeting("hello"))]
struct GreetingLive;

#[derive(ProviderSpecDerive)]
#[provides(Audit, with = |deps: &Env| Audit(deps.len() as u32))]
struct AuditLive;

#[derive(Debug, PartialEq)]
struct AppError;

/// The environment, named once.
type AppCaps = caps!(Greeting, Audit);

/// An environment-free step (like an `EffectLogger` call).
fn note(_msg: &'static str) -> Effect<(), AppError> {
  succeed(())
}

/// An infallible service call.
fn greet() -> Effect<&'static str, Never, caps!(Greeting)> {
  effect!(|r| { (~Greeting).0 })
}

fn app() -> Effect<String, AppError, AppCaps> {
  effect!(|r: &mut AppCaps| {
    ~note("start");
    let audit = ~Audit;
    let n = audit.0;
    let greeting = ~greet().widen_error::<AppError>();
    format!("{greeting}:{n}")
  })
}

#[test]
fn alias_annotation_unit_env_steps_and_widen_error_compose() {
  let out = run_with([provide!(GreetingLive), provide!(AuditLive)], app()).expect("run");
  assert!(out.starts_with("hello:"), "{out}");
}

#[test]
fn widen_error_in_tail_position_infers_the_error() {
  let program: Effect<&'static str, AppError, caps!(Greeting)> =
    effect!(|r| { ~greet().widen_error() });
  let out = run_with([provide!(GreetingLive)], program).expect("run");
  assert_eq!(out, "hello");
}

#[test]
fn widen_error_keeps_real_failures_of_other_steps() {
  let program: Effect<(), AppError, caps!(Greeting)> = effect!(|r| {
    let _ = ~greet().widen_error::<AppError>();
    ~id_effect::fail::<(), AppError, ()>(AppError);
  });
  assert_eq!(
    run_blocking(program, <caps!(Greeting)>::from_env(build_greeting_env())),
    Err(AppError)
  );
}

fn build_greeting_env() -> Env {
  id_effect::build_env([provide!(GreetingLive)]).expect("env")
}

#[test]
fn unit_env_step_binds_in_env_and_unit_bodies() {
  let in_env: Effect<(), AppError, Env> = effect!(|r| {
    ~note("in Env");
  });
  assert_eq!(run_blocking(in_env, Env::new()), Ok(()));
  let in_unit: Effect<(), AppError, ()> = effect!(|r| {
    ~note("in ()");
  });
  assert_eq!(run_blocking(in_unit, ()), Ok(()));
}

#[test]
fn missing_key_lookup_panics_naming_service_and_this_file() {
  let program: Effect<u32, AppError, AppCaps> = effect!(|r| { (~Audit).0 });
  // Only `Greeting` is present: the environment is built by hand to bypass `run_with` verification.
  let mut env = Env::new();
  env.insert::<id_effect::Cap<Greeting>>(Greeting("hi"));
  let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    run_blocking(program, AppCaps::from_env(env))
  }))
  .expect_err("missing capability must panic");
  let msg = panic.downcast_ref::<String>().cloned().unwrap_or_default();
  assert!(msg.contains("missing capability"), "{msg}");
  assert!(msg.contains("Audit"), "{msg}");
  assert!(msg.contains(file!()), "{msg}");
}

#[test]
fn missing_key_while_narrowing_panics_naming_the_tilde_call_site() {
  let program: Effect<&'static str, AppError, caps!(Audit)> =
    effect!(|r| { ~greet().widen_error() });
  let mut env = Env::new();
  env.insert::<id_effect::Cap<Audit>>(Audit(1));
  let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    run_blocking(program, <caps!(Audit)>::from_env(env))
  }))
  .expect_err("narrowing to a missing capability must panic");
  let msg = panic.downcast_ref::<String>().cloned().unwrap_or_default();
  assert!(msg.contains("Greeting"), "{msg}");
  assert!(msg.contains("required by `~`"), "{msg}");
  assert!(msg.contains(file!()), "{msg}");
}

#[test]
fn run_with_reports_missing_capability_by_name() {
  let err = run_with([provide!(GreetingLive)], app()).expect_err("Audit is not provided");
  let id_effect::RunError::Capability(err) = err else {
    panic!("expected a capability error, got {err:?}");
  };
  assert_eq!(err.location(), None, "verification has no single call site");
  let shown = err.to_string();
  assert!(shown.contains("missing capability"), "{shown}");
  assert!(shown.contains("Audit"), "{shown}");
}
