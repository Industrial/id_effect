//! Dylint lints for idiomatic id_effect code (`Effect<A, E, R>`, `effect!`, services, fibers,
//! time, raw SQL). Run them with `cargo dylint` (see the README and the book's appendix D).
//!
//! Every lint is `Warn` by default except `tests_must_use_run_test` (`Allow`); escalate one with
//! `DYLINT_RUSTFLAGS="-D <lint>"` or `#![cfg_attr(dylint_lib = "id_effect_lints", deny(<lint>))]`.
//! `effect_map_identity`, `effect_flat_map_succeed`, `effect_flat_map_then` and the wrapped-literal
//! case of `raw_sql_non_literal` carry MachineApplicable suggestions that `cargo dylint --fix`
//! applies.
#![feature(rustc_private)]
#![warn(unused_extern_crates)]

dylint_linting::dylint_library!();

extern crate rustc_ast;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_lint;
extern crate rustc_middle;
extern crate rustc_session;
extern crate rustc_span;

mod diag;
mod lints;
mod utils;

use lints::{combinators, exprs, fn_sigs, items, sql};
use rustc_lint::{LateContext, LateLintPass, LintStore};
use rustc_session::{Session, declare_lint, declare_lint_pass};

// ══════════════════════════════════════════════════════════════════════════════
//  Lint declarations – Category A: Function Signatures
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub NO_ASYNC_FN_IN_EFFECT_CRATE,
    Warn,
    "use `fn -> Effect<A, E, R>` + `from_async` instead of `async fn`"
}

declare_lint! {
    pub NO_RESULT_RETURN_IN_PUB_FN,
    Warn,
    "public functions must return `Effect<A, E, R>` rather than `Result<T, E>`"
}

declare_lint! {
    pub EFFECT_FN_REQUIRES_WHERE_CLAUSE,
    Warn,
    "Effect-returning functions with generic parameters must have a `where` clause"
}

declare_lint! {
    pub EFFECT_ERROR_BOUND_MUST_USE_FROM,
    Warn,
    "`E` in Effect-returning functions must include `E: From<ConcreteError>` bounds"
}

declare_lint! {
    pub NO_CONCRETE_CONTEXT_IN_PUB_API,
    Warn,
    "public APIs must not expose a 0.6 `Context<Cons<…>>` as `R`; spell `R` with `caps!(Key, …)`"
}

declare_lint! {
    pub NO_CONCRETE_ENV_IN_PUB_API,
    Warn,
    "public APIs must not expose a 0.6 `Env` as `R`; spell `R` with `caps!(Key, …)`"
}

declare_lint! {
    pub PREFER_NEEDS_X_OVER_RAW_GET_BOUNDS,
    Warn,
    "use `caps!(Key, …)` / `R: Selector<Key, I>` instead of 0.6 `Get<Key, Here, Target = T>` bounds"
}

declare_lint! {
    pub NO_EFFECT_SUFFIX_ON_GRAPH_BUILDERS,
    Warn,
    "Effect-returning functions must not have the `_effect` suffix"
}

declare_lint! {
    pub BLOCKING_SUFFIX_RESERVED_FOR_RUNNERS,
    Warn,
    "`*_blocking` names must call `block_on` internally; not return `Effect`"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category B: effect! Macro discipline
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub MULTI_STEP_MUST_USE_EFFECT_MACRO,
    Warn,
    "chains of 2+ `.flat_map` calls should use `effect!({ ~… })` do-notation"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category C: Async / .await
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub NO_AWAIT_OUTSIDE_FROM_ASYNC,
    Warn,
    "`.await` must only appear inside `async` blocks passed to `from_async`"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category D: Runtime execution boundaries
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub RUN_BLOCKING_ONLY_AT_PROGRAM_EDGE,
    Warn,
    "`block_on`/`try_block_on` must only be called from binary entry points"
}

// 0.7 has no `run_test`: `block_on` is the test edge too. Opt-in (Allow) nudge towards a
// `TestExecutor` for deterministic, virtual-time tests.
declare_lint! {
    pub TESTS_MUST_USE_RUN_TEST,
    Allow,
    "time-dependent test effects should run on a `TestExecutor` (virtual time), not `block_on`"
}

declare_lint! {
    pub NO_TOKIO_SPAWN,
    Warn,
    "use `.fork()` / `Runtime::fork` instead of `tokio::spawn`"
}

declare_lint! {
    pub NO_THREAD_SPAWN,
    Warn,
    "use `.fork()` / `FiberHandle` instead of `std::thread::spawn`"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category E: Services / dependency injection
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub NO_STATIC_SERVICE_GLOBALS,
    Warn,
    "service types must not live in `static` globals; list them in `R` (`caps!(Key, …)`) and provide them with a `Layer`"
}

declare_lint! {
    pub NO_THREAD_LOCAL_FOR_REQUEST_CONTEXT,
    Warn,
    "use `FiberRef` for fiber-scoped state instead of `thread_local!`"
}

declare_lint! {
    pub PROVIDE_AT_EDGE_NOT_IN_LIBRARY,
    Warn,
    "library functions must not call `.provide(…)`; provide at the program edge"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category F: Error handling
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub NO_NESTED_RESULT_IN_PUB_API,
    Warn,
    "avoid `Result<Result<_,_>,_>` in public APIs; use `Or<E1,E2>` or flat errors"
}

declare_lint! {
    pub NO_ERROR_COLLAPSE_TO_STRING,
    Warn,
    "avoid `Err(format!(…))` / `Err(e.to_string())`; use typed error variants"
}

declare_lint! {
    pub NO_UNWRAP_INSIDE_EFFECT,
    Warn,
    "avoid `.unwrap()` / `.expect()` in Effect-returning functions; use typed propagation"
}

declare_lint! {
    pub NO_PANIC_INSIDE_EFFECT,
    Warn,
    "avoid `panic!` / `unreachable!` in Effect-returning functions; use `fail(…)`"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category G: Concurrency and resources
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub FORK_MUST_BE_JOINED,
    Warn,
    "every `.fork()` result must be bound and `.join()`-ed or `.interrupt()`-ed"
}

declare_lint! {
    pub NO_MUTEX_FOR_FIBER_COORDINATION,
    Warn,
    "use `SynchronizedRef` or `TRef`/`Stm` (`id_effect_fp`) instead of `Arc<Mutex<…>>` across fibers"
}

declare_lint! {
    pub NO_RC_REFCELL_IN_EFFECT_CODE,
    Warn,
    "`Rc<RefCell<…>>` is unsafe across fibers; use `Ref` / `SynchronizedRef`"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category H: Time
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub NO_THREAD_SLEEP_IN_DOMAIN,
    Warn,
    "use `Schedule`-based combinators instead of `std::thread::sleep`"
}

declare_lint! {
    pub NO_SYSTEM_TIME_IN_DOMAIN,
    Warn,
    "read time from the executor clock (`UtcDateTime::now()`) instead of `SystemTime::now()` etc."
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category I: Testing
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub TEST_MUST_NOT_USE_THREAD_SLEEP,
    Warn,
    "advance a `TestExecutor`'s virtual clock in tests instead of `std::thread::sleep`"
}

declare_lint! {
    pub RUN_TEST_RESULT_MUST_BE_MATCHED,
    Warn,
    "`exit.into_result().unwrap()` – use `assert!(matches!(exit, Exit::Success(_)))` instead"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category J: Observability
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub NO_PRINTLN_IN_EFFECT_GRAPH,
    Warn,
    "use structured spans / tracing hooks instead of `println!` in Effect code"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category K: Schema
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    pub SCHEMA_PARSING_AT_IO_BOUNDARY,
    Warn,
    "use `Unknown` + `Schema` pipeline instead of bare `serde_json::from_value(…).unwrap()`"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category L: Combinators (MachineApplicable suggestions)
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    /// ### What it does
    /// Flags `effect.map(|x| x)` and `effect.map_err(|e| e)` on an `Effect`.
    ///
    /// ### Why is this bad?
    /// The call allocates a continuation that changes nothing.
    ///
    /// ### Fix
    /// Removes the call (MachineApplicable).
    pub EFFECT_MAP_IDENTITY,
    Warn,
    "`.map(|x| x)` / `.map_err(|e| e)` on an `Effect` does nothing"
}

declare_lint! {
    /// ### What it does
    /// Flags `effect.flat_map(|x| succeed(f(x)))` on an `Effect`.
    ///
    /// ### Why is this bad?
    /// A continuation that only wraps its result in `succeed` is a `map`.
    ///
    /// ### Fix
    /// Rewrites it to `effect.map(|x| f(x))` (MachineApplicable).
    pub EFFECT_FLAT_MAP_SUCCEED,
    Warn,
    "`.flat_map(|x| succeed(…))` on an `Effect` is `.map(|x| …)`"
}

declare_lint! {
    /// ### What it does
    /// Flags `effect.flat_map(move |_| next)` on an `Effect`, where `next` is a local effect.
    ///
    /// ### Why is this bad?
    /// `Effect::then` says the same thing without a closure.
    ///
    /// ### Fix
    /// Rewrites it to `effect.then(next)` (MachineApplicable).
    pub EFFECT_FLAT_MAP_THEN,
    Warn,
    "`.flat_map(move |_| next)` on an `Effect` is `.then(next)`"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Category M: Raw SQL (ADR 0013 R5, 0.11.0 D9)
// ══════════════════════════════════════════════════════════════════════════════

declare_lint! {
    /// ### What it does
    /// Flags a call to the `&str` methods `query`, `execute` or `query_stream` of the
    /// `id_effect_sql` traits `SqlClient`, `SqlTransaction` and `SqlExecutor` whose SQL argument
    /// is not a string literal, a `const`/`static`, a local bound once to one of those, a
    /// `&'static str` parameter, or an `id_effect_sql::Sql`'s `as_str()`.
    ///
    /// ### Why is this bad?
    /// The effect-level `query`/`execute` only accept `impl Into<Sql>` (a `&'static str`), so a
    /// `format!`ed statement does not compile there; the trait methods are the driver boundary
    /// and still take any `&str`. SQL built at run time can splice untrusted input into a
    /// statement. Bind values as parameters; for SQL that really is built at run time use
    /// `query_raw`/`execute_raw` with a `RawSql`, which need the `RawSqlCapability`.
    ///
    /// ### Known problems
    /// Calls inside `id_effect_sql` itself and forwarding calls inside an `impl` of one of the
    /// traits are not linted (they are the boundary).
    ///
    /// ### Fix
    /// `&"…".to_string()`, `&String::from("…")` and `&"…".to_owned()` become the literal
    /// (MachineApplicable); anything else needs a person.
    pub RAW_SQL_NON_LITERAL,
    Warn,
    "SQL passed to a `SqlClient` / `SqlTransaction` `&str` method is not a literal or a `const`"
}

// ══════════════════════════════════════════════════════════════════════════════
//  Combined lint pass
// ══════════════════════════════════════════════════════════════════════════════

declare_lint_pass!(EffectRsPass => [
    NO_ASYNC_FN_IN_EFFECT_CRATE,
    NO_RESULT_RETURN_IN_PUB_FN,
    EFFECT_FN_REQUIRES_WHERE_CLAUSE,
    EFFECT_ERROR_BOUND_MUST_USE_FROM,
    NO_CONCRETE_CONTEXT_IN_PUB_API,
    NO_CONCRETE_ENV_IN_PUB_API,
    PREFER_NEEDS_X_OVER_RAW_GET_BOUNDS,
    NO_EFFECT_SUFFIX_ON_GRAPH_BUILDERS,
    BLOCKING_SUFFIX_RESERVED_FOR_RUNNERS,
    MULTI_STEP_MUST_USE_EFFECT_MACRO,
    NO_AWAIT_OUTSIDE_FROM_ASYNC,
    RUN_BLOCKING_ONLY_AT_PROGRAM_EDGE,
    TESTS_MUST_USE_RUN_TEST,
    NO_TOKIO_SPAWN,
    NO_THREAD_SPAWN,
    NO_STATIC_SERVICE_GLOBALS,
    NO_THREAD_LOCAL_FOR_REQUEST_CONTEXT,
    PROVIDE_AT_EDGE_NOT_IN_LIBRARY,
    NO_NESTED_RESULT_IN_PUB_API,
    NO_ERROR_COLLAPSE_TO_STRING,
    NO_UNWRAP_INSIDE_EFFECT,
    NO_PANIC_INSIDE_EFFECT,
    FORK_MUST_BE_JOINED,
    NO_MUTEX_FOR_FIBER_COORDINATION,
    NO_RC_REFCELL_IN_EFFECT_CODE,
    NO_THREAD_SLEEP_IN_DOMAIN,
    NO_SYSTEM_TIME_IN_DOMAIN,
    TEST_MUST_NOT_USE_THREAD_SLEEP,
    RUN_TEST_RESULT_MUST_BE_MATCHED,
    NO_PRINTLN_IN_EFFECT_GRAPH,
    SCHEMA_PARSING_AT_IO_BOUNDARY,
    EFFECT_MAP_IDENTITY,
    EFFECT_FLAT_MAP_SUCCEED,
    EFFECT_FLAT_MAP_THEN,
    RAW_SQL_NON_LITERAL,
]);

impl<'tcx> LateLintPass<'tcx> for EffectRsPass {
  // ── Function-level checks (A-rules + B-01) ────────────────────────────────

  fn check_fn(
    &mut self,
    cx: &LateContext<'tcx>,
    kind: rustc_hir::intravisit::FnKind<'tcx>,
    decl: &'tcx rustc_hir::FnDecl<'tcx>,
    _body: &'tcx rustc_hir::Body<'tcx>,
    span: rustc_span::Span,
    def_id: rustc_hir::def_id::LocalDefId,
  ) {
    let is_pub = cx.tcx.visibility(def_id).is_public();
    let in_test = utils::in_test_context(cx, def_id);

    // A-01: no async fn
    fn_sigs::check_no_async_fn(cx, kind, span, def_id);

    // A-02: no Result return in pub fn
    fn_sigs::check_no_result_return_pub(cx, kind, decl, is_pub, span, def_id);

    // F-01 (fn bodies): no nested Result in public fn return type
    items::check_no_nested_result_fn(cx, decl, is_pub, span);

    // A-03, A-04, A-05, A-06: where clause and bound rules (skip for tests)
    if !in_test {
      let generics = extract_generics(kind);
      if let Some(generics) = generics {
        fn_sigs::check_effect_fn_where_clause(cx, decl, generics, span);
        fn_sigs::check_effect_error_from_bound(cx, decl, generics, span);
        fn_sigs::check_no_concrete_context(cx, decl, is_pub, span);
        fn_sigs::check_prefer_needs_x(cx, decl, generics, span);
      }
    }

    // A-07: no _effect suffix
    fn_sigs::check_no_effect_suffix(cx, kind, decl, span);

    // A-08: _blocking suffix misuse
    fn_sigs::check_blocking_suffix_misuse(cx, kind, decl, span);
  }

  // ── Expression-level checks ───────────────────────────────────────────────

  fn check_expr(&mut self, cx: &LateContext<'tcx>, expr: &'tcx rustc_hir::Expr<'tcx>) {
    let in_effect_fn = current_fn_returns_effect(cx, expr);
    let def_id = cx.tcx.hir_get_parent_item(expr.hir_id).def_id;

    // B-01: chained flat_map
    fn_sigs::check_multi_step_flat_map(cx, expr);

    // C-01: .await outside from_async
    exprs::check_no_await_outside_from_async(cx, expr);

    // D-01 + D-02: block_on placement
    exprs::check_run_blocking_at_edge(cx, expr, def_id);
    exprs::check_tests_use_run_test(cx, expr, def_id);

    // D-03: no tokio::spawn
    exprs::check_no_tokio_spawn(cx, expr);

    // D-04: no thread::spawn
    exprs::check_no_thread_spawn(cx, expr);

    // E-04: .provide() in library
    exprs::check_provide_at_edge(cx, expr, def_id);

    // F-02: Err(format!()) / Err(e.to_string())
    exprs::check_no_error_collapse_to_string(cx, expr);

    // F-03: .unwrap() in Effect fn
    exprs::check_no_unwrap_in_effect(cx, expr, in_effect_fn);

    // F-04: panic! in Effect fn
    exprs::check_no_panic_in_effect(cx, expr, in_effect_fn);

    // H-01 + I-01: thread::sleep
    exprs::check_no_thread_sleep(cx, expr, def_id);

    // H-02: SystemTime::now()
    exprs::check_no_system_time_now(cx, expr);

    // I-02: exit.into_result().unwrap()
    exprs::check_run_test_result_matched(cx, expr);

    // J-01: println! in Effect fn
    exprs::check_no_println_in_effect(cx, expr, in_effect_fn);

    // K-01: serde_json raw parse
    exprs::check_schema_parsing_at_boundary(cx, expr);

    // L-01..L-03: combinator rewrites with MachineApplicable suggestions
    combinators::check_map_identity(cx, expr);
    combinators::check_flat_map_succeed(cx, expr);
    combinators::check_flat_map_then(cx, expr);

    // M-01: non-literal SQL into a SqlClient / SqlTransaction `&str` method
    sql::check_raw_sql_non_literal(cx, expr);
  }

  // ── Statement-level checks ────────────────────────────────────────────────

  fn check_stmt(&mut self, cx: &LateContext<'tcx>, stmt: &'tcx rustc_hir::Stmt<'tcx>) {
    // G-01: discarded .fork()
    exprs::check_fork_result_discarded(cx, stmt);
  }

  // ── Item-level checks ─────────────────────────────────────────────────────

  fn check_item(&mut self, cx: &LateContext<'tcx>, item: &'tcx rustc_hir::Item<'tcx>) {
    // E-02: static service globals
    items::check_no_static_service_globals(cx, item);
    // E-03: thread_local!
    items::check_no_thread_local(cx, item);
    // F-01: nested Result in type alias
    items::check_no_nested_result_type(cx, item);
  }

  // ── Type-level checks ─────────────────────────────────────────────────────

  fn check_ty(
    &mut self,
    cx: &LateContext<'tcx>,
    ty: &'tcx rustc_hir::Ty<'tcx, rustc_hir::AmbigArg>,
  ) {
    // G-03: Arc<Mutex<…>>
    exprs::check_no_mutex_for_fiber_coordination(cx, ty);
    // G-04: Rc<RefCell<…>>
    exprs::check_no_rc_refcell(cx, ty);
  }
}

// ══════════════════════════════════════════════════════════════════════════════
//  register_lints – Dylint entry point
// ══════════════════════════════════════════════════════════════════════════════

#[unsafe(no_mangle)]
pub fn register_lints(_sess: &Session, store: &mut LintStore) {
  store.register_lints(&[
    NO_ASYNC_FN_IN_EFFECT_CRATE,
    NO_RESULT_RETURN_IN_PUB_FN,
    EFFECT_FN_REQUIRES_WHERE_CLAUSE,
    EFFECT_ERROR_BOUND_MUST_USE_FROM,
    NO_CONCRETE_CONTEXT_IN_PUB_API,
    NO_CONCRETE_ENV_IN_PUB_API,
    PREFER_NEEDS_X_OVER_RAW_GET_BOUNDS,
    NO_EFFECT_SUFFIX_ON_GRAPH_BUILDERS,
    BLOCKING_SUFFIX_RESERVED_FOR_RUNNERS,
    MULTI_STEP_MUST_USE_EFFECT_MACRO,
    NO_AWAIT_OUTSIDE_FROM_ASYNC,
    RUN_BLOCKING_ONLY_AT_PROGRAM_EDGE,
    TESTS_MUST_USE_RUN_TEST,
    NO_TOKIO_SPAWN,
    NO_THREAD_SPAWN,
    NO_STATIC_SERVICE_GLOBALS,
    NO_THREAD_LOCAL_FOR_REQUEST_CONTEXT,
    PROVIDE_AT_EDGE_NOT_IN_LIBRARY,
    NO_NESTED_RESULT_IN_PUB_API,
    NO_ERROR_COLLAPSE_TO_STRING,
    NO_UNWRAP_INSIDE_EFFECT,
    NO_PANIC_INSIDE_EFFECT,
    FORK_MUST_BE_JOINED,
    NO_MUTEX_FOR_FIBER_COORDINATION,
    NO_RC_REFCELL_IN_EFFECT_CODE,
    NO_THREAD_SLEEP_IN_DOMAIN,
    NO_SYSTEM_TIME_IN_DOMAIN,
    TEST_MUST_NOT_USE_THREAD_SLEEP,
    RUN_TEST_RESULT_MUST_BE_MATCHED,
    NO_PRINTLN_IN_EFFECT_GRAPH,
    SCHEMA_PARSING_AT_IO_BOUNDARY,
    EFFECT_MAP_IDENTITY,
    EFFECT_FLAT_MAP_SUCCEED,
    EFFECT_FLAT_MAP_THEN,
    RAW_SQL_NON_LITERAL,
  ]);
  store.register_late_lint_pass(Box::new(|_| Box::new(EffectRsPass)));
}

// The directories are absolute: compiletest rewrites every occurrence of the directory string to
// `$DIR` in the expected output, and a bare `ui` would also hit lint names such as `requires`.

/// One UI test per lint (`ui/<lint>.rs` + `.stderr`; `.fixed` for the MachineApplicable ones).
#[test]
fn ui() {
  dylint_testing::ui::Test::src_base(
    env!("CARGO_PKG_NAME"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/ui"),
  )
  .rustc_flags(["--edition=2024"])
  .run();
}

/// The lints that look at test functions, compiled with `--test`.
#[test]
fn ui_test_context() {
  dylint_testing::ui::Test::src_base(
    env!("CARGO_PKG_NAME"),
    concat!(env!("CARGO_MANIFEST_DIR"), "/ui_test"),
  )
  .rustc_flags(["--edition=2024", "--test"])
  .run();
}

// ══════════════════════════════════════════════════════════════════════════════
//  Private helpers
// ══════════════════════════════════════════════════════════════════════════════

/// Extract generics from `FnKind` for free functions (item-level).
fn extract_generics<'tcx>(
  kind: rustc_hir::intravisit::FnKind<'tcx>,
) -> Option<&'tcx rustc_hir::Generics<'tcx>> {
  match kind {
    rustc_hir::intravisit::FnKind::ItemFn(_, generics, _) => Some(generics),
    // Method generics come from the containing impl/trait; not available via FnKind::Method.
    rustc_hir::intravisit::FnKind::Method(..) | rustc_hir::intravisit::FnKind::Closure => None,
  }
}

/// Returns `true` if the closest enclosing fn/method returns `Effect<_, _, _>`.
fn current_fn_returns_effect<'tcx>(cx: &LateContext<'tcx>, expr: &rustc_hir::Expr<'tcx>) -> bool {
  let mut hir_id = expr.hir_id;
  loop {
    hir_id = match cx.tcx.hir_parent_id_iter(hir_id).next() {
      Some(p) => p,
      None => return false,
    };
    match cx.tcx.hir_node(hir_id) {
      rustc_hir::Node::Item(item) => {
        if let rustc_hir::ItemKind::Fn { sig, .. } = &item.kind {
          return utils::returns_effect(sig.decl);
        }
        return false;
      }
      rustc_hir::Node::TraitItem(ti) => {
        if let rustc_hir::TraitItemKind::Fn(sig, _) = &ti.kind {
          return utils::returns_effect(sig.decl);
        }
        return false;
      }
      rustc_hir::Node::ImplItem(ii) => {
        if let rustc_hir::ImplItemKind::Fn(sig, _) = &ii.kind {
          return utils::returns_effect(sig.decl);
        }
        return false;
      }
      rustc_hir::Node::Expr(e) => {
        if let rustc_hir::ExprKind::Closure(cl) = &e.kind {
          return utils::returns_effect(cl.fn_decl);
        }
        // Continue walking up for nested expressions / blocks.
      }
      _ => {}
    }
  }
}
