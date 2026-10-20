use rustc_hir::intravisit::FnKind;
use rustc_hir::{FnDecl, FnRetTy, IsAsync, QPath, TyKind, WherePredicateKind};
use rustc_lint::LateContext;
use rustc_span::Symbol;

// ──────────────────────────────────────────────────────────
// Return-type helpers
// ──────────────────────────────────────────────────────────

/// `true` iff the HIR type's outermost named segment is `name`.
pub fn ty_last_segment_is(ty: &rustc_hir::Ty<'_>, name: &str) -> bool {
  match &ty.kind {
    TyKind::Path(QPath::Resolved(_, path)) => path
      .segments
      .last()
      .is_some_and(|s| s.ident.name.as_str() == name),
    TyKind::Path(QPath::TypeRelative(_, seg)) => seg.ident.name.as_str() == name,
    _ => false,
  }
}

pub fn returns_effect(decl: &FnDecl<'_>) -> bool {
  matches!(&decl.output, FnRetTy::Return(ty) if ty_last_segment_is(ty, "Effect"))
}

pub fn returns_result(decl: &FnDecl<'_>) -> bool {
  matches!(&decl.output, FnRetTy::Return(ty) if ty_last_segment_is(ty, "Result"))
}

// ──────────────────────────────────────────────────────────
// Async / coroutine helpers
// ──────────────────────────────────────────────────────────

pub fn fn_header_is_async(header: &rustc_hir::FnHeader) -> bool {
  matches!(header.asyncness, IsAsync::Async(_))
}

pub fn is_async_fn_kind(kind: &FnKind<'_>) -> bool {
  matches!(kind, FnKind::ItemFn(_, _, h) if fn_header_is_async(h))
}

// ──────────────────────────────────────────────────────────
// Test-context helpers
// ──────────────────────────────────────────────────────────

/// `true` inside a `#[test]` function or a `#[cfg(test)]` item (module, impl, fn).
///
/// After expansion `#[test]` is gone: the test harness leaves the function and a sibling
/// `const <name>: test::TestDescAndFn`, which is what this looks for (as Clippy does).
pub fn in_test_context(cx: &LateContext<'_>, def_id: rustc_hir::def_id::LocalDefId) -> bool {
  let tcx = cx.tcx;
  let hir_id = tcx.local_def_id_to_hir_id(def_id);
  std::iter::once(hir_id)
    .chain(tcx.hir_parent_id_iter(hir_id))
    .any(|id| is_cfg_test(cx, id) || is_test_fn(cx, id))
}

/// `true` iff the node carries `#[cfg(test)]` (kept as a parsed `CfgTrace`).
fn is_cfg_test(cx: &LateContext<'_>, id: rustc_hir::HirId) -> bool {
  fn mentions_test(cfg: &rustc_ast::attr::data_structures::CfgEntry) -> bool {
    use rustc_ast::attr::data_structures::CfgEntry;
    match cfg {
      CfgEntry::All(all, _) => all.iter().any(mentions_test),
      CfgEntry::NameValue {
        name, value: None, ..
      } => name.as_str() == "test",
      _ => false,
    }
  }
  rustc_hir::find_attr!(
    cx.tcx.hir_attrs(id),
    CfgTrace(cfgs) if cfgs.iter().any(|(cfg, _)| mentions_test(cfg))
  )
}

/// `true` iff `id` is a function the test harness registered (`#[test]`).
fn is_test_fn(cx: &LateContext<'_>, id: rustc_hir::HirId) -> bool {
  let tcx = cx.tcx;
  let rustc_hir::Node::Item(item) = tcx.hir_node(id) else {
    return false;
  };
  let rustc_hir::ItemKind::Fn { ident, .. } = item.kind else {
    return false;
  };
  tcx
    .hir_module_free_items(tcx.parent_module(id))
    .any(|sibling| match tcx.hir_item(sibling).kind {
      rustc_hir::ItemKind::Const(c, _, ty, _) => {
        c.name == ident.name && ty_last_segment_is(ty, "TestDescAndFn")
      }
      _ => false,
    })
}

// ──────────────────────────────────────────────────────────
// Where-clause helpers
// ──────────────────────────────────────────────────────────

pub fn has_where_clause(generics: &rustc_hir::Generics<'_>) -> bool {
  !generics.predicates.is_empty()
}

pub fn generic_param_names(generics: &rustc_hir::Generics<'_>) -> Vec<Symbol> {
  generics
    .params
    .iter()
    .filter_map(|p| match p.kind {
      rustc_hir::GenericParamKind::Type { .. } => Some(p.name.ident().name),
      _ => None,
    })
    .collect()
}

/// `true` iff the where clause contains an `E: From<_>` bound for `e_name`.
pub fn e_has_from_bound(generics: &rustc_hir::Generics<'_>, e_name: Symbol) -> bool {
  use rustc_hir::GenericBound;
  for pred in generics.predicates {
    if let WherePredicateKind::BoundPredicate(bp) = pred.kind
      && ty_last_segment_is(bp.bounded_ty, e_name.as_str())
    {
      for bound in bp.bounds {
        if let GenericBound::Trait(tref) = bound
          && tref
            .trait_ref
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident.name.as_str() == "From")
        {
          return true;
        }
      }
    }
  }
  false
}

/// `true` iff any where predicate uses a raw `Get<…>` bound.
pub fn has_raw_get_bound(generics: &rustc_hir::Generics<'_>) -> bool {
  use rustc_hir::GenericBound;
  for pred in generics.predicates {
    if let WherePredicateKind::BoundPredicate(bp) = pred.kind {
      for bound in bp.bounds {
        if let GenericBound::Trait(tref) = bound
          && tref
            .trait_ref
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident.name.as_str() == "Get")
        {
          return true;
        }
      }
    }
  }
  false
}

// ──────────────────────────────────────────────────────────
// Expression / macro helpers
// ──────────────────────────────────────────────────────────

/// `true` iff the expression is a macro call whose name contains `name`.
pub fn is_macro_call_named(expr: &rustc_hir::Expr<'_>, name: &str) -> bool {
  if expr.span.from_expansion() {
    let data = expr.span.ctxt().outer_expn_data();
    if let rustc_span::hygiene::ExpnKind::Macro(_, sym) = data.kind {
      return sym.as_str().contains(name);
    }
  }
  false
}

// ──────────────────────────────────────────────────────────
// Type-checked helpers (combinator and SQL lints)
// ──────────────────────────────────────────────────────────

/// `true` iff `ty` (through references) is an ADT named `name` (`Effect`, `Sql`, …).
pub fn ty_is_adt_named(cx: &LateContext<'_>, ty: rustc_middle::ty::Ty<'_>, name: &str) -> bool {
  match ty.peel_refs().kind() {
    rustc_middle::ty::Adt(def, _) => cx.tcx.item_name(def.did()).as_str() == name,
    _ => false,
  }
}

/// `true` iff the expression's type is an ADT named `name`.
pub fn expr_ty_is_adt_named(cx: &LateContext<'_>, expr: &rustc_hir::Expr<'_>, name: &str) -> bool {
  cx.typeck_results
    .is_some_and(|tr| ty_is_adt_named(cx, tr.expr_ty(expr), name))
}

/// The value of `expr` through `{ … }` blocks that hold only a tail expression.
pub fn peel_blocks<'tcx>(expr: &'tcx rustc_hir::Expr<'tcx>) -> &'tcx rustc_hir::Expr<'tcx> {
  match expr.kind {
    rustc_hir::ExprKind::Block(block, None)
      if block.stmts.is_empty()
        && matches!(block.rules, rustc_hir::BlockCheckMode::DefaultBlock) =>
    {
      block.expr.map_or(expr, peel_blocks)
    }
    _ => expr,
  }
}

/// The source text of `span`, if it has one.
pub fn snippet(cx: &LateContext<'_>, span: rustc_span::Span) -> Option<String> {
  use rustc_lint::LintContext;
  cx.sess().source_map().span_to_snippet(span).ok()
}

/// The call site of a `name!(…)` (one of `names`) whose expansion `expr` is the outermost node
/// of: once per macro call, and at a span in the user's code (a lint emitted inside a std macro's
/// expansion is suppressed as external).
pub fn outermost_macro_call(
  cx: &LateContext<'_>,
  expr: &rustc_hir::Expr<'_>,
  names: &[&str],
) -> Option<rustc_span::Span> {
  if !names.iter().any(|n| is_macro_call_named(expr, n)) {
    return None;
  }
  // The nearest enclosing expression (statements in between may come from the expansion too).
  let enclosing = cx
    .tcx
    .hir_parent_iter(expr.hir_id)
    .find_map(|(_, node)| match node {
      rustc_hir::Node::Expr(e) => Some(e.span.ctxt()),
      _ => None,
    });
  if enclosing == Some(expr.span.ctxt()) {
    return None;
  }
  Some(expr.span.source_callsite())
}

pub fn contains_rc_refcell(ty: &rustc_hir::Ty<'_>) -> bool {
  if !ty_last_segment_is(ty, "Rc") {
    return false;
  }
  if let TyKind::Path(QPath::Resolved(_, path)) = &ty.kind
    && let Some(last) = path.segments.last()
    && let Some(args) = last.args
  {
    for arg in args.args {
      if let rustc_hir::GenericArg::Type(inner) = arg
        && ty_last_segment_is(inner.as_unambig_ty(), "RefCell")
      {
        return true;
      }
    }
  }
  false
}
