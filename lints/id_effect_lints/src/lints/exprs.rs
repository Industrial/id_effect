use crate::diag::span_lint_and_help;
use crate::utils::*;
use rustc_hir::ExprKind;
use rustc_lint::LateContext;

use crate::{
  FORK_MUST_BE_JOINED, NO_AWAIT_OUTSIDE_FROM_ASYNC, NO_ERROR_COLLAPSE_TO_STRING,
  NO_MUTEX_FOR_FIBER_COORDINATION, NO_PANIC_INSIDE_EFFECT, NO_PRINTLN_IN_EFFECT_GRAPH,
  NO_RC_REFCELL_IN_EFFECT_CODE, NO_SYSTEM_TIME_IN_DOMAIN, NO_THREAD_SLEEP_IN_DOMAIN,
  NO_THREAD_SPAWN, NO_TOKIO_SPAWN, NO_UNWRAP_INSIDE_EFFECT, PROVIDE_AT_EDGE_NOT_IN_LIBRARY,
  RUN_BLOCKING_ONLY_AT_PROGRAM_EDGE, RUN_TEST_RESULT_MUST_BE_MATCHED,
  SCHEMA_PARSING_AT_IO_BOUNDARY, TEST_MUST_NOT_USE_THREAD_SLEEP, TESTS_MUST_USE_RUN_TEST,
};

// ─── C-01: No .await outside from_async ───────────────────────────────────────

/// Flags `.await` unless it sits inside a call to `from_async(…)` (the closure or async block
/// passed to it).
pub fn check_no_await_outside_from_async(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) {
  use rustc_hir::{MatchSource, Node};
  // HIR lowers `x.await` to `match into_future(x) { … loop { match poll(…) { … } } }`: both
  // `match`es are `AwaitDesugar` and end where `.await` ends, so only the outer one counts.
  let is_await =
    |e: &rustc_hir::Expr<'_>| matches!(e.kind, ExprKind::Match(_, _, MatchSource::AwaitDesugar));
  if !is_await(expr) {
    return;
  }
  for (_, node) in cx.tcx.hir_parent_iter(expr.hir_id) {
    let Node::Expr(parent) = node else {
      continue;
    };
    if is_await(parent) && parent.span.hi() == expr.span.hi() {
      return; // the inner `match` of the same `.await`
    }
    if is_call_to_name(parent, "from_async") {
      return;
    }
  }
  span_lint_and_help(
    cx,
    NO_AWAIT_OUTSIDE_FROM_ASYNC,
    expr.span,
    "`.await` used outside of a `from_async` wrapper",
    None,
    "wrap async calls with `from_async(move || async move { expr.await })`, \
           or bind them with `~` inside an `effect!({ … })` body",
  );
}

// ─── D-01: block_on only at program edge ───────────────────────────────────────

pub fn check_run_blocking_at_edge(
  cx: &LateContext<'_>,
  expr: &rustc_hir::Expr<'_>,
  def_id: rustc_hir::def_id::LocalDefId,
) {
  if !is_block_on_call(expr) {
    return;
  }
  if in_test_context(cx, def_id) {
    return; // handled by D-02
  }
  // Allow in `main`.
  if is_in_main(cx, def_id) {
    return;
  }
  span_lint_and_help(
    cx,
    RUN_BLOCKING_ONLY_AT_PROGRAM_EDGE,
    expr.span,
    "`block_on` / `try_block_on` called outside a binary entry point",
    None,
    "move this call to `main` or a dedicated runner; library functions return `Effect` \
             (or `.await` `Runtime::run(effect)` in async code)",
  );
}

// ─── D-02: Time-dependent tests run on a TestExecutor ─────────────────────────

pub fn check_tests_use_run_test(
  cx: &LateContext<'_>,
  expr: &rustc_hir::Expr<'_>,
  def_id: rustc_hir::def_id::LocalDefId,
) {
  if !is_block_on_call(expr) {
    return;
  }
  if in_test_context(cx, def_id) {
    span_lint_and_help(
      cx,
      TESTS_MUST_USE_RUN_TEST,
      expr.span,
      "`block_on` used inside a test function",
      None,
      "run time-dependent effects on a `TestExecutor` (virtual time, `advance(d)`) \
             for deterministic tests",
    );
  }
}

// ─── D-03: No tokio::spawn ────────────────────────────────────────────────────

pub fn check_no_tokio_spawn(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) {
  if let ExprKind::Call(callee, _) = &expr.kind
    && path_segments_match(callee, &["tokio", "spawn"])
  {
    span_lint_and_help(
      cx,
      NO_TOKIO_SPAWN,
      expr.span,
      "`tokio::spawn` used in Effect code",
      None,
      "use `my_effect.fork()` (a `FiberHandle`) or `Runtime::fork` for structured fibers",
    );
  }
}

// ─── D-04: No thread::spawn ───────────────────────────────────────────────────

pub fn check_no_thread_spawn(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) {
  if let ExprKind::Call(callee, _) = &expr.kind
    && path_segments_match(callee, &["thread", "spawn"])
  {
    span_lint_and_help(
      cx,
      NO_THREAD_SPAWN,
      expr.span,
      "`std::thread::spawn` used in Effect code",
      None,
      "use `effect.fork()` and its `FiberHandle` (`join` / `interrupt`) for structured \
                 concurrency",
    );
  }
}

// ─── E-04: .provide() in library function ─────────────────────────────────────

pub fn check_provide_at_edge(
  cx: &LateContext<'_>,
  expr: &rustc_hir::Expr<'_>,
  def_id: rustc_hir::def_id::LocalDefId,
) {
  if let ExprKind::MethodCall(seg, _, _, _) = &expr.kind {
    if seg.ident.name.as_str() != "provide" {
      return;
    }
    if in_test_context(cx, def_id) || is_in_main(cx, def_id) {
      return;
    }
    span_lint_and_help(
      cx,
      PROVIDE_AT_EDGE_NOT_IN_LIBRARY,
      expr.span,
      "`.provide(…)` called inside a library function",
      None,
      "provide dependencies at the program edge (`main` or tests); \
             library functions should declare requirements in `R`",
    );
  }
}

// ─── F-02: No Err(format!()) / Err(e.to_string()) ────────────────────────────

pub fn check_no_error_collapse_to_string(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) {
  if let ExprKind::Call(callee, args) = &expr.kind {
    // Check for `Err(…)` constructor.
    if !path_last_segment_is_name(callee, "Err") {
      return;
    }
    if let Some(arg) = args.first() {
      let is_format = is_macro_call_named(arg, "format");
      let is_to_string = matches!(
          &arg.kind,
          ExprKind::MethodCall(seg, _, _, _) if seg.ident.name.as_str() == "to_string"
      );
      if is_format || is_to_string {
        span_lint_and_help(
          cx,
          NO_ERROR_COLLAPSE_TO_STRING,
          expr.span,
          "error value collapsed to `String` at effect boundary",
          None,
          "use a typed error variant instead of `Err(format!(…))` or `Err(e.to_string())`",
        );
      }
    }
  }
}

// ─── F-03: No .unwrap() / .expect() in Effect code ────────────────────────────

pub fn check_no_unwrap_in_effect(
  cx: &LateContext<'_>,
  expr: &rustc_hir::Expr<'_>,
  in_effect_fn: bool,
) {
  if !in_effect_fn {
    return;
  }
  if let ExprKind::MethodCall(seg, _, _, _) = &expr.kind {
    let name = seg.ident.name.as_str();
    if name == "unwrap" || name == "expect" {
      span_lint_and_help(
        cx,
        NO_UNWRAP_INSIDE_EFFECT,
        expr.span,
        format!("`.{name}()` used inside an Effect-returning function"),
        None,
        "use typed error propagation: `?`, `~sub` (widens via `From`), `.map_err(…)`, \
                 or `fail(MyError::…)`",
      );
    }
  }
}

// ─── F-04: No panic! / unreachable! in Effect functions ───────────────────────

pub fn check_no_panic_in_effect(
  cx: &LateContext<'_>,
  expr: &rustc_hir::Expr<'_>,
  in_effect_fn: bool,
) {
  if !in_effect_fn {
    return;
  }
  if let Some(call_site) = outermost_macro_call(cx, expr, &["panic", "unreachable"]) {
    span_lint_and_help(
      cx,
      NO_PANIC_INSIDE_EFFECT,
      call_site,
      "`panic!` / `unreachable!` used inside an Effect-returning function",
      None,
      "use `fail(MyError::…)` for expected failures; \
             reserve `panic!` only for true logic invariant violations at the top level",
    );
  }
}

// ─── G-01: Fork must be joined ────────────────────────────────────────────────
// Detection is in `check_fork_result_discarded` (statement-level); see `check_stmt`.

/// Called from `check_stmt` – flag a `.fork()` result discarded as a statement.
pub fn check_fork_result_discarded(cx: &LateContext<'_>, stmt: &rustc_hir::Stmt<'_>) {
  use rustc_hir::StmtKind;
  if let StmtKind::Expr(expr) | StmtKind::Semi(expr) = stmt.kind
    && let ExprKind::MethodCall(seg, _, _, _) = &expr.kind
    && seg.ident.name.as_str() == "fork"
  {
    span_lint_and_help(
      cx,
      FORK_MUST_BE_JOINED,
      expr.span,
      "`.fork()` result discarded; fiber will be untracked",
      None,
      "bind to a `let handle = …` and call `handle.join()` or `handle.interrupt()`",
    );
  }
}

// ─── G-03: No Arc<Mutex> for fiber coordination ───────────────────────────────

pub fn check_no_mutex_for_fiber_coordination(
  cx: &LateContext<'_>,
  ty: &rustc_hir::Ty<'_, rustc_hir::AmbigArg>,
) {
  use rustc_hir::{GenericArg, TyKind};
  let ty = ty.as_unambig_ty();
  if let TyKind::Path(rustc_hir::QPath::Resolved(_, path)) = &ty.kind
    && let Some(last) = path.segments.last()
    && last.ident.name.as_str() == "Arc"
    && let Some(args) = last.args
  {
    for arg in args.args {
      if let GenericArg::Type(inner) = arg
        && (ty_last_segment_is(inner.as_unambig_ty(), "Mutex")
          || ty_last_segment_is(inner.as_unambig_ty(), "RwLock"))
      {
        span_lint_and_help(
          cx,
          NO_MUTEX_FOR_FIBER_COORDINATION,
          ty.span,
          "`Arc<Mutex<…>>` / `Arc<RwLock<…>>` used in Effect code",
          None,
          "use `SynchronizedRef` / `Ref`, or `TRef` / `Stm` transactions \
                                     (`id_effect_fp`), for shared state across fibers",
        );
      }
    }
  }
}

// ─── G-04: No Rc<RefCell> in Effect code ──────────────────────────────────────

pub fn check_no_rc_refcell(cx: &LateContext<'_>, ty: &rustc_hir::Ty<'_, rustc_hir::AmbigArg>) {
  if contains_rc_refcell(ty.as_unambig_ty()) {
    span_lint_and_help(
      cx,
      NO_RC_REFCELL_IN_EFFECT_CODE,
      ty.as_unambig_ty().span,
      "`Rc<RefCell<…>>` found in Effect code",
      None,
      "use `Ref` / `SynchronizedRef` from the coordination module for shared cells",
    );
  }
}

// ─── H-01: No thread::sleep in domain code ────────────────────────────────────

pub fn check_no_thread_sleep(
  cx: &LateContext<'_>,
  expr: &rustc_hir::Expr<'_>,
  def_id: rustc_hir::def_id::LocalDefId,
) {
  if let ExprKind::Call(callee, _) = &expr.kind
    && path_segments_match(callee, &["thread", "sleep"])
  {
    let in_test = in_test_context(cx, def_id);
    let lint = if in_test {
      TEST_MUST_NOT_USE_THREAD_SLEEP
    } else {
      NO_THREAD_SLEEP_IN_DOMAIN
    };
    span_lint_and_help(
      cx,
      lint,
      expr.span,
      "`std::thread::sleep` used in Effect code",
      None,
      if in_test {
        "run the effect on a `TestExecutor` and `advance(d)` its virtual clock"
      } else {
        "use `sleep(d)` or `effect.retry(Schedule::exponential(…))` on the executor clock"
      },
    );
  }
}

// ─── H-02: No SystemTime::now() in domain ─────────────────────────────────────

pub fn check_no_system_time_now(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) {
  if let ExprKind::Call(callee, _) = &expr.kind
    && (path_segments_match(callee, &["SystemTime", "now"])
      || path_segments_match(callee, &["Utc", "now"])
      || path_segments_match(callee, &["Local", "now"]))
  {
    span_lint_and_help(
      cx,
      NO_SYSTEM_TIME_IN_DOMAIN,
      expr.span,
      "direct time access (`SystemTime::now()` / `Utc::now()`) in domain code",
      None,
      "read time from the executor clock: `~UtcDateTime::now()` inside `effect!` \
                 (virtual on a `TestExecutor`)",
    );
  }
}

// ─── I-02: a test's Exit must be matched ──────────────────────────────────────

pub fn check_run_test_result_matched(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) {
  if let ExprKind::MethodCall(seg, recv, _, _) = &expr.kind {
    if seg.ident.name.as_str() != "unwrap" {
      return;
    }
    // Check if the receiver is `exit.into_result()`.
    if is_call_to_name(recv, "into_result") {
      span_lint_and_help(
        cx,
        RUN_TEST_RESULT_MUST_BE_MATCHED,
        expr.span,
        "`exit.into_result().unwrap()` called",
        None,
        "match the `Exit` instead (`assert!(matches!(effect.block_on(), Exit::Success(_)))`) \
                 to distinguish a typed failure from a defect or an interrupt",
      );
    }
  }
}

// ─── J-01: No println! in Effect code ────────────────────────────────────────

pub fn check_no_println_in_effect(
  cx: &LateContext<'_>,
  expr: &rustc_hir::Expr<'_>,
  in_effect_fn: bool,
) {
  if !in_effect_fn {
    return;
  }
  if let Some(call_site) = outermost_macro_call(cx, expr, &["println", "eprintln"]) {
    span_lint_and_help(
      cx,
      NO_PRINTLN_IN_EFFECT_GRAPH,
      call_site,
      "`println!` / `eprintln!` inside an Effect-returning function",
      None,
      "use `effect.with_span(\"step\")` or a span hook; \
             remove debug prints before committing",
    );
  }
}

// ─── K-01: Schema parsing at IO boundary ─────────────────────────────────────

pub fn check_schema_parsing_at_boundary(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>) {
  // Detect `serde_json::from_value(…).unwrap()` or `from_str(…).unwrap()`.
  if let ExprKind::MethodCall(seg, recv, _, _) = &expr.kind {
    if seg.ident.name.as_str() != "unwrap" && seg.ident.name.as_str() != "expect" {
      return;
    }
    if let ExprKind::Call(callee, _) = &recv.kind
      && (path_segments_match(callee, &["serde_json", "from_value"])
        || path_segments_match(callee, &["serde_json", "from_str"]))
    {
      span_lint_and_help(
        cx,
        SCHEMA_PARSING_AT_IO_BOUNDARY,
        expr.span,
        "`serde_json::from_value(…).unwrap()` in domain code",
        None,
        "parse at IO boundaries using `Unknown::from_serde_json(v)` + `Schema`",
      );
    }
  }
}

// ─── Local helpers ────────────────────────────────────────────────────────────

/// `true` for a `block_on` / `try_block_on` call (`Effect`, `LocalEffect` or `Runtime`).
fn is_block_on_call(expr: &rustc_hir::Expr<'_>) -> bool {
  is_call_to_name(expr, "block_on") || is_call_to_name(expr, "try_block_on")
}

/// `true` if the free function / method call is named `name` (last path segment).
fn is_call_to_name(expr: &rustc_hir::Expr<'_>, name: &str) -> bool {
  match &expr.kind {
    ExprKind::Call(callee, _) => path_last_segment_is_name(callee, name),
    ExprKind::MethodCall(seg, _, _, _) => seg.ident.name.as_str() == name,
    _ => false,
  }
}

/// `true` if the callee path ends with a segment named `name`.
fn path_last_segment_is_name(expr: &rustc_hir::Expr<'_>, name: &str) -> bool {
  if let ExprKind::Path(rustc_hir::QPath::Resolved(_, path)) = &expr.kind {
    return path
      .segments
      .last()
      .is_some_and(|s| s.ident.name.as_str() == name);
  }
  false
}

/// `true` iff the callee's path contains all `segments` (as a sub-sequence).
fn path_segments_match(callee: &rustc_hir::Expr<'_>, segments: &[&str]) -> bool {
  use rustc_hir::QPath;
  let names: Vec<&str> = match &callee.kind {
    ExprKind::Path(QPath::Resolved(_, path)) => path
      .segments
      .iter()
      .map(|s| s.ident.name.as_str())
      .collect(),
    // `SystemTime::now()`: the type's path, then the associated item.
    ExprKind::Path(QPath::TypeRelative(ty, seg)) => match &ty.kind {
      rustc_hir::TyKind::Path(QPath::Resolved(_, path)) => path
        .segments
        .iter()
        .chain(std::iter::once(*seg))
        .map(|s| s.ident.name.as_str())
        .collect(),
      _ => return false,
    },
    _ => return false,
  };
  names.windows(segments.len()).any(|w| w == segments)
}

/// `true` iff `def_id`'s function is named `main`.
fn is_in_main(cx: &LateContext<'_>, def_id: rustc_hir::def_id::LocalDefId) -> bool {
  let hir_id = cx.tcx.local_def_id_to_hir_id(def_id);
  if let rustc_hir::Node::Item(item) = cx.tcx.hir_node(hir_id)
    && let rustc_hir::ItemKind::Fn { ident, .. } = &item.kind
  {
    return ident.name.as_str() == "main";
  }
  false
}
