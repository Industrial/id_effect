//! Category L: `Effect` combinator rewrites. Each lint carries one MachineApplicable suggestion,
//! so `cargo dylint --fix` applies it. Code from macro expansions (`effect!`) is skipped.

use crate::diag::span_lint_and_sugg;
use crate::utils::*;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::{Expr, ExprKind, PatKind, PathSegment, QPath};
use rustc_lint::LateContext;

use crate::{EFFECT_FLAT_MAP_SUCCEED, EFFECT_FLAT_MAP_THEN, EFFECT_MAP_IDENTITY};

// ─── L-01: .map(|x| x) / .map_err(|e| e) ─────────────────────────────────────

pub fn check_map_identity<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
  let Some((seg, recv, closure)) = effect_method_with_closure(cx, expr, &["map", "map_err"]) else {
    return;
  };
  let body = cx.tcx.hir_body(closure.body);
  let [param] = body.params else {
    return;
  };
  let PatKind::Binding(_, binding, _, None) = param.pat.kind else {
    return;
  };
  let value = peel_blocks(body.value);
  if !matches!(
    value.kind,
    ExprKind::Path(QPath::Resolved(None, path)) if path.res == Res::Local(binding)
  ) || recv.span.from_expansion()
  {
    return;
  }
  let name = seg.ident.name;
  span_lint_and_sugg(
    cx,
    EFFECT_MAP_IDENTITY,
    expr.span,
    format!("`.{name}(|x| x)` on an `Effect` does nothing"),
    format!("remove the `.{name}(…)` call"),
    vec![(expr.span.with_lo(recv.span.hi()), String::new())],
  );
}

// ─── L-02: .flat_map(|x| succeed(f(x))) → .map(|x| f(x)) ─────────────────────

pub fn check_flat_map_succeed<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
  let Some((seg, _, closure)) = effect_method_with_closure(cx, expr, &["flat_map"]) else {
    return;
  };
  let value = peel_blocks(cx.tcx.hir_body(closure.body).value);
  let ExprKind::Call(callee, [inner]) = value.kind else {
    return;
  };
  let ExprKind::Path(QPath::Resolved(None, path)) = callee.kind else {
    return;
  };
  let Some(last) = path.segments.last() else {
    return;
  };
  // A turbofish (`succeed::<u64, _, _>(0)`) may carry the only type annotation: leave it.
  let has_turbofish = last
    .args
    .is_some_and(|a| !a.args.is_empty() || !a.constraints.is_empty());
  if last.ident.name.as_str() != "succeed"
    || has_turbofish
    || !matches!(path.res, Res::Def(DefKind::Fn, _))
    || !expr_ty_is_adt_named(cx, value, "Effect")
    || value.span.from_expansion()
    || inner.span.from_expansion()
  {
    return;
  }
  let Some(inner_src) = snippet(cx, inner.span) else {
    return;
  };
  span_lint_and_sugg(
    cx,
    EFFECT_FLAT_MAP_SUCCEED,
    expr.span,
    "`.flat_map(|x| succeed(…))` on an `Effect` is a `.map`",
    "use `.map` and return the value",
    vec![(seg.ident.span, "map".to_string()), (value.span, inner_src)],
  );
}

// ─── L-03: .flat_map(move |_| next) → .then(next) ────────────────────────────

pub fn check_flat_map_then<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
  let Some((seg, _, closure)) = effect_method_with_closure(cx, expr, &["flat_map"]) else {
    return;
  };
  let body = cx.tcx.hir_body(closure.body);
  let [param] = body.params else {
    return;
  };
  if !matches!(param.pat.kind, PatKind::Wild) {
    return;
  }
  let value = peel_blocks(body.value);
  // Only a captured local: building any other expression eagerly could change when it runs.
  if !matches!(
    value.kind,
    ExprKind::Path(QPath::Resolved(None, path)) if matches!(path.res, Res::Local(_))
  ) || value.span.from_expansion()
  {
    return;
  }
  let Some(next) = snippet(cx, value.span) else {
    return;
  };
  span_lint_and_sugg(
    cx,
    EFFECT_FLAT_MAP_THEN,
    expr.span,
    "`.flat_map(move |_| next)` on an `Effect` is `.then(next)`",
    "use `.then`",
    vec![(
      seg.ident.span.with_hi(expr.span.hi()),
      format!("then({next})"),
    )],
  );
}

// ─── Local helpers ────────────────────────────────────────────────────────────

/// `recv.<name>(<closure>)` on an `Effect`, outside macro expansions.
fn effect_method_with_closure<'tcx>(
  cx: &LateContext<'tcx>,
  expr: &'tcx Expr<'tcx>,
  names: &[&str],
) -> Option<(
  &'tcx PathSegment<'tcx>,
  &'tcx Expr<'tcx>,
  &'tcx rustc_hir::Closure<'tcx>,
)> {
  if expr.span.from_expansion() {
    return None;
  }
  let ExprKind::MethodCall(seg, recv, [arg], _) = expr.kind else {
    return None;
  };
  if !names.contains(&seg.ident.name.as_str()) || !expr_ty_is_adt_named(cx, recv, "Effect") {
    return None;
  }
  let ExprKind::Closure(closure) = arg.kind else {
    return None;
  };
  if arg.span.from_expansion() {
    return None;
  }
  Some((seg, recv, closure))
}
