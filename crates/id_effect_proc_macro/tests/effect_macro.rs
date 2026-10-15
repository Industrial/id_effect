//! The token-level `effect!` / `effect_local!` / `caps!` expansions (`src/effect_macro`): each
//! test pins one parser branch — the head forms, which `~` operands are capability reads and
//! which are binds (`is_cap_read`, `expr_end`), the rewrite of nested groups, parallel joins,
//! error widening and `caps!` list building.
//!
//! The kernel-level acceptance tests for the same macros live in `id_effect/tests/macros.rs`
//! and `id_effect/tests/join_binds.rs`; compile-fail diagnostics in the `ui` trybuild suite.

use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use id_effect::{Cause, Exit, Never};
use id_effect::{
  Effect, HCons, HNil, LocalEffect, caps, effect, effect_local, fail, hlist, need, succeed,
};

// ── fixtures ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Counter(usize);

#[derive(Debug)]
struct Name(&'static str);

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

fn one() -> Effect<u32> {
  succeed(1)
}

fn two() -> Effect<u32> {
  succeed(2)
}

fn boom() -> Effect<u32, DbError> {
  fail(DbError("boom"))
}

fn count() -> Effect<usize, Never, caps!(Counter)> {
  effect!({ ~Counter.0 })
}

/// An upper-case type followed by `::` is a path, not a capability read.
struct Make;

impl Make {
  fn five() -> Effect<u32> {
    succeed(5)
  }
}

/// An upper-case identifier followed by `(` is a call, not a capability read.
#[allow(non_snake_case)]
fn Seven() -> Effect<u32> {
  succeed(7)
}

struct Src;

impl Src {
  fn default_of<T: Default + Send + 'static>(&self) -> Effect<T> {
    succeed(T::default())
  }
}

struct Pair(Effect<u32>, Effect<u32>);

macro_rules! three {
  () => {
    succeed::<u32, Never, HNil>(3)
  };
}

fn ok<A: Send + 'static>(exit: Exit<A, impl Send + 'static>) -> A {
  match exit {
    Exit::Success(a) => a,
    Exit::Failure(_) => panic!("expected success"),
  }
}

// ── caps! ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn caps_of_nothing_is_hnil() {
  same_type(PhantomData::<caps!()>, PhantomData::<HNil>);
}

#[test]
fn caps_of_one_is_a_single_cons() {
  same_type(
    PhantomData::<caps!(Counter)>,
    PhantomData::<HCons<Counter, HNil>>,
  );
}

#[test]
fn caps_keeps_the_written_order() {
  same_type(
    PhantomData::<caps!(Name, Counter)>,
    PhantomData::<HCons<Name, HCons<Counter, HNil>>>,
  );
}

#[test]
fn caps_accepts_a_trailing_comma() {
  same_type(
    PhantomData::<caps!(Counter,)>,
    PhantomData::<HCons<Counter, HNil>>,
  );
}

#[test]
fn caps_keeps_generic_arguments_with_commas_as_one_item() {
  same_type(
    PhantomData::<caps!(std::collections::HashMap<u8, u16>, Counter)>,
    PhantomData::<HCons<std::collections::HashMap<u8, u16>, HCons<Counter, HNil>>>,
  );
}

#[test]
fn caps_does_not_split_at_a_fn_pointer_arrow() {
  same_type(
    PhantomData::<caps!(fn(u8) -> u16, Counter)>,
    PhantomData::<HCons<fn(u8) -> u16, HCons<Counter, HNil>>>,
  );
}

// ── head forms ────────────────────────────────────────────────────────────────────────────

#[test]
fn a_bare_block_body_is_the_success_value() {
  let eff: Effect<u8> = effect!({ 40 + 2 });
  assert_eq!(ok(eff.block_on()), 42);
}

#[test]
fn an_unbraced_body_is_accepted() {
  let eff: Effect<u8> = effect!(9);
  assert_eq!(ok(eff.block_on()), 9);
}

#[test]
fn an_empty_closure_head_is_ignored() {
  let eff: Effect<u8> = effect!(|| { 3 });
  assert_eq!(ok(eff.block_on()), 3);
}

#[test]
fn a_named_head_binds_the_environment() {
  let eff: Effect<&'static str, Never, caps!(Name)> = effect!(|r| {
    let env: &caps!(Name) = r;
    (env.0).0
  });
  assert_eq!(ok(eff.provide(Name("n")).block_on()), "n");
}

#[test]
fn a_typed_head_fixes_the_environment_type() {
  let eff = effect!(|r: &caps!(Name, Counter)| { (r.1).0.0 + (r.0).0.len() });
  let eff: Effect<usize, Never, caps!(Name, Counter)> = eff;
  assert_eq!(
    ok(eff.provide_env(hlist![Name("ab"), Counter(3)]).block_on()),
    5
  );
}

#[test]
fn a_mut_typed_head_strips_the_mut() {
  let eff: Effect<usize, Never, caps!(Counter)> = effect!(|r: &mut caps!(Counter)| { (r.0).0 });
  assert_eq!(ok(eff.provide(Counter(4)).block_on()), 4);
}

#[test]
fn an_underscore_head_binds_nothing_but_keeps_the_type() {
  let eff = effect!(|_: &caps!(Counter)| { ~Counter.0 * 2 });
  let eff: Effect<usize, Never, caps!(Counter)> = eff;
  assert_eq!(ok(eff.provide(Counter(4)).block_on()), 8);
}

// ── `~` operands: capability reads ──────────────────────────────────────────────────────────

#[test]
fn a_tilde_capability_is_a_shared_reference_to_it() {
  let eff: Effect<usize, Never, caps!(Counter)> = effect!({
    let c: Arc<Counter> = Arc::clone(~Counter);
    c.0
  });
  assert_eq!(ok(eff.provide(Counter(6)).block_on()), 6);
}

#[test]
fn a_capability_read_takes_field_and_method_postfixes() {
  let eff: Effect<usize, Never, caps!(Name)> = effect!({ ~Name.0.len() });
  assert_eq!(ok(eff.provide(Name("four")).block_on()), 4);
}

#[test]
fn a_capability_read_picks_its_list_index() {
  let eff: Effect<String, Never, caps!(Name, Counter)> =
    effect!({ format!("{}{}", ~Name.0, ~Counter.0) });
  assert_eq!(
    ok(eff.provide_env(hlist![Name("x"), Counter(1)]).block_on()),
    "x1"
  );
}

// ── `~` operands: binds (`expr_end`) ─────────────────────────────────────────────────────────

#[test]
fn an_upper_case_path_is_a_bind_not_a_read() {
  let eff: Effect<u32> = effect!({ ~Make::five() });
  assert_eq!(ok(eff.block_on()), 5);
}

#[test]
fn an_upper_case_call_is_a_bind_not_a_read() {
  let eff: Effect<u32> = effect!({ ~Seven() });
  assert_eq!(ok(eff.block_on()), 7);
}

#[test]
fn a_bind_through_a_turbofish_function() {
  let eff: Effect<u32> = effect!({ ~succeed::<u32, Never, HNil>(11) });
  assert_eq!(ok(eff.block_on()), 11);
}

#[test]
fn a_bind_through_a_macro_call() {
  let eff: Effect<u32> = effect!({ ~three!() + 1 });
  assert_eq!(ok(eff.block_on()), 4);
}

#[test]
fn a_bind_through_a_method_turbofish() {
  let src = Src;
  let eff: Effect<u8> = effect!({ ~src.default_of::<u8>() + 1 });
  assert_eq!(ok(eff.block_on()), 1);
}

#[test]
fn a_bind_through_indexing_then_a_call() {
  let effs: [fn() -> Effect<u32>; 2] = [one, two];
  let eff: Effect<u32> = effect!({ ~effs[1]() * 10 });
  assert_eq!(ok(eff.block_on()), 20);
}

#[test]
fn a_bind_through_a_tuple_field() {
  let eff: Effect<u32> = effect!({
    let pair = Pair(one(), two());
    ~pair.0 * 10 + ~pair.1
  });
  assert_eq!(ok(eff.block_on()), 12);
}

#[test]
fn a_bind_of_a_fallible_constructor_with_question_mark() {
  fn pick(ok: bool) -> Result<Effect<u32, DbError>, DbError> {
    if ok {
      Ok(succeed(8))
    } else {
      Err(DbError("no effect"))
    }
  }
  let good: Effect<u32, DbError> = effect!({ ~pick(true)? });
  assert_eq!(ok(good.block_on()), 8);
  let bad: Effect<u32, DbError> = effect!({ ~pick(false)? });
  assert_eq!(
    bad.block_on(),
    Exit::Failure(Cause::Fail(DbError("no effect")))
  );
}

#[test]
fn a_parenthesised_single_effect_is_a_plain_bind() {
  let eff: Effect<u32> = effect!({ ~(two()) + 1 });
  assert_eq!(ok(eff.block_on()), 3);
}

#[test]
fn a_bind_runs_a_sub_environment_effect() {
  let eff: Effect<usize, Never, caps!(Name, Counter)> = effect!({ ~count() + 1 });
  assert_eq!(
    ok(eff.provide_env(hlist![Name("n"), Counter(2)]).block_on()),
    3
  );
}

// ── rewrite of nested groups ─────────────────────────────────────────────────────────────────

#[test]
fn binds_inside_if_and_match_are_rewritten() {
  let eff: Effect<u32> = effect!({
    let a = if ~one() == 1 { ~two() } else { 0 };
    match ~two() {
      2 => a + 10,
      _ => 0,
    }
  });
  assert_eq!(ok(eff.block_on()), 12);
}

#[test]
fn binds_inside_tuples_arrays_and_loops_are_rewritten() {
  let eff: Effect<u32> = effect!({
    let (a, b) = (~one(), ~two());
    let xs = [~one(), ~two()];
    let mut sum = 0;
    for x in xs {
      sum += x + ~one();
    }
    a + b + sum
  });
  assert_eq!(ok(eff.block_on()), 8);
}

#[test]
fn binds_inside_other_macro_arguments_are_rewritten() {
  let eff: Effect<String> = effect!({ format!("{}-{}", ~one(), ~two()) });
  assert_eq!(ok(eff.block_on()), "1-2");
}

#[test]
fn a_nested_effect_keeps_its_own_tildes() {
  let eff: Effect<usize, Never, caps!(Counter)> = effect!({
    let inner: Effect<usize, Never, caps!(Counter)> = effect!({ ~Counter.0 * 100 });
    ~inner + ~Counter.0
  });
  assert_eq!(ok(eff.provide(Counter(2)).block_on()), 202);
}

// ── error widening ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_never_bind_widens_into_any_error() {
  let eff: Effect<u32, DbError> = effect!({ ~one() + 1 });
  assert_eq!(ok(eff.block_on()), 2);
}

#[test]
fn a_typed_bind_widens_through_from() {
  let eff: Effect<u32, AppError> = effect!({ ~boom() });
  assert_eq!(
    eff.block_on(),
    Exit::Failure(Cause::Fail(AppError::Db(DbError("boom"))))
  );
}

// ── parallel joins ──────────────────────────────────────────────────────────────────────────

#[test]
fn a_join_of_two_returns_the_values_in_order() {
  let eff: Effect<(u32, u32)> = effect!({ ~(two(), one()) });
  assert_eq!(ok(eff.block_on()), (2, 1));
}

#[test]
fn a_join_of_eight_is_the_maximum_arity() {
  let eff: Effect<u32> = effect!({
    let (a, b, c, d, e, f, g, h) = ~(one(), one(), one(), one(), one(), one(), one(), two());
    a + b + c + d + e + f + g + h
  });
  assert_eq!(ok(eff.block_on()), 9);
}

#[test]
fn a_join_accepts_a_trailing_comma() {
  let eff: Effect<(u32, u32)> = effect!({ ~(one(), two(),) });
  assert_eq!(ok(eff.block_on()), (1, 2));
}

#[test]
fn a_join_branch_error_widens_and_fails_the_join() {
  let eff: Effect<u32, AppError> = effect!({
    let (a, b) = ~(one(), boom());
    a + b
  });
  assert_eq!(
    eff.block_on(),
    Exit::Failure(Cause::Fail(AppError::Db(DbError("boom"))))
  );
}

#[test]
fn join_branches_are_rewritten_and_see_the_environment() {
  let eff: Effect<usize, Never, caps!(Counter)> = effect!({
    let (a, b) = ~(count(), need::<Counter>().map::<_, Never, _, _>(|c| c.0 * 10).same_env());
    a + b
  });
  assert_eq!(ok(eff.provide(Counter(3)).block_on()), 33);
}

// ── effect_local! ────────────────────────────────────────────────────────────────────────────

#[test]
fn effect_local_holds_non_send_values_across_binds() {
  let eff: LocalEffect<usize, Never, caps!(Counter)> = effect_local!({
    let held = Rc::new(10_usize);
    let n = ~count();
    *held + n
  });
  assert_eq!(ok(eff.provide(Counter(1)).block_on()), 11);
}

#[test]
fn effect_local_takes_a_named_head_and_widens_errors() {
  let eff: LocalEffect<usize, AppError, caps!(Counter)> = effect_local!(|r| {
    let env: &caps!(Counter) = r;
    let base = (env.0).0;
    base + ~boom() as usize
  });
  assert_eq!(
    eff.provide(Counter(1)).block_on(),
    Exit::Failure(Cause::Fail(AppError::Db(DbError("boom"))))
  );
}

#[test]
fn effect_local_can_contain_a_nested_effect() {
  let eff: LocalEffect<u32, Never, HNil> = effect_local!({
    let inner: Effect<u32> = effect!({ ~two() * 2 });
    ~inner + 1
  });
  assert_eq!(ok(eff.block_on()), 5);
}
