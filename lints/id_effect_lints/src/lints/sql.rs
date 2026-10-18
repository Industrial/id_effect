//! Category M: raw SQL (ADR 0013 R5's second enforcement point, 0.11.0 D9). The effect-level
//! `query`/`execute` take `impl Into<Sql>` and reject a `String` at compile time; the driver
//! traits' `&str` methods cannot, so this lint checks their SQL argument instead.

use crate::diag::{span_lint_and_help, span_lint_and_sugg};
use crate::utils::*;
use rustc_ast::LitKind;
use rustc_hir::def::{DefKind, Res};
use rustc_hir::def_id::{DefId, LOCAL_CRATE};
use rustc_hir::{BindingMode, Expr, ExprKind, HirId, Node, PatKind, QPath, UnOp};
use rustc_lint::LateContext;

use crate::RAW_SQL_NON_LITERAL;

/// The `id_effect_sql` traits whose methods take the SQL text as a `&str`.
const SQL_TRAITS: &[&str] = &["SqlClient", "SqlTransaction", "SqlExecutor"];
/// Their methods whose first argument is the SQL text.
const SQL_METHODS: &[&str] = &["query", "execute", "query_stream"];
/// The crate that defines the traits: its own calls are the boundary.
const SQL_CRATE: &str = "id_effect_sql";

// ─── M-01: raw_sql_non_literal ────────────────────────────────────────────────

pub fn check_raw_sql_non_literal<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) {
  let Some(typeck) = cx.typeck_results else {
    return;
  };
  let (method, sql) = match expr.kind {
    // `client.query(sql, &params)`
    ExprKind::MethodCall(_, _, [sql, ..], _) => (typeck.type_dependent_def_id(expr.hir_id), sql),
    // `SqlClient::query(&client, sql, &params)`
    ExprKind::Call(callee, [_, sql, ..]) => match &callee.kind {
      ExprKind::Path(qpath) => (cx.qpath_res(qpath, callee.hir_id).opt_def_id(), sql),
      _ => return,
    },
    _ => return,
  };
  let Some(method) = method else {
    return;
  };
  if !SQL_METHODS.contains(&cx.tcx.item_name(method).as_str())
    || sql_trait_of(cx, method).is_none()
    || cx.tcx.crate_name(LOCAL_CRATE).as_str() == SQL_CRATE
    || in_sql_trait_impl(cx, expr.hir_id)
    || is_static_sql(cx, sql)
  {
    return;
  }
  let msg = "SQL passed to a `SqlClient` / `SqlTransaction` `&str` method is not a literal or a \
             `const`";
  if let Some(lit) = wrapped_literal(sql)
    && let Some(src) = snippet(cx, lit.span)
    && !sql.span.from_expansion()
  {
    span_lint_and_sugg(
      cx,
      RAW_SQL_NON_LITERAL,
      sql.span,
      msg,
      "pass the literal itself",
      vec![(sql.span, src)],
    );
    return;
  }
  span_lint_and_help(
    cx,
    RAW_SQL_NON_LITERAL,
    sql.span,
    msg,
    None,
    "bind values as parameters (`$1`, `params![…]`) and pass a literal or a `const`; SQL that \
     really is built at run time goes through `id_effect_sql::query_raw` / `execute_raw` with a \
     `RawSql`, which need the `RawSqlCapability` (ADR 0013 R5)",
  );
}

// ─── Local helpers ────────────────────────────────────────────────────────────

/// The trait (one of [`SQL_TRAITS`]) that `method` belongs to, directly or through an impl.
fn sql_trait_of(cx: &LateContext<'_>, method: DefId) -> Option<DefId> {
  let tcx = cx.tcx;
  let trait_id = tcx.trait_of_assoc(method).or_else(|| {
    tcx
      .trait_impl_of_assoc(method)
      .map(|imp| tcx.impl_trait_id(imp))
  })?;
  SQL_TRAITS
    .contains(&tcx.item_name(trait_id).as_str())
    .then_some(trait_id)
}

/// `true` inside a method of an `impl SqlClient for …` (or the other traits): a wrapper that
/// forwards its own `sql` argument is the boundary, its callers are linted.
fn in_sql_trait_impl(cx: &LateContext<'_>, hir_id: HirId) -> bool {
  let owner = cx.tcx.hir_get_parent_item(hir_id).to_def_id();
  cx.tcx
    .trait_impl_of_assoc(owner)
    .is_some_and(|imp| SQL_TRAITS.contains(&cx.tcx.item_name(cx.tcx.impl_trait_id(imp)).as_str()))
}

/// Through `&`, `*` and temporaries.
fn peel_refs<'tcx>(mut expr: &'tcx Expr<'tcx>) -> &'tcx Expr<'tcx> {
  loop {
    expr = match expr.kind {
      ExprKind::AddrOf(_, _, inner) | ExprKind::Unary(UnOp::Deref, inner) => inner,
      ExprKind::DropTemps(inner) => inner,
      _ => return expr,
    };
  }
}

/// `true` for SQL fixed at compile time: a string literal (also from `concat!`/`include_str!`),
/// a `const`/`static`, an immutable local bound to one of those, a `&'static str` parameter, or
/// `sql.as_str()` on an `id_effect_sql::Sql` (only `&'static str` converts into one).
fn is_static_sql<'tcx>(cx: &LateContext<'tcx>, expr: &'tcx Expr<'tcx>) -> bool {
  let expr = peel_refs(expr);
  match expr.kind {
    ExprKind::Lit(lit) => matches!(lit.node, LitKind::Str(..)),
    ExprKind::Path(ref qpath) => match cx.qpath_res(qpath, expr.hir_id) {
      Res::Def(DefKind::Const { .. } | DefKind::AssocConst { .. } | DefKind::Static { .. }, _) => {
        true
      }
      Res::Local(id) => {
        immutable_local_init(cx, id).is_some_and(|init| is_static_sql(cx, init))
          || is_static_str_param(cx, id)
      }
      _ => false,
    },
    ExprKind::MethodCall(seg, recv, [], _) => {
      matches!(seg.ident.name.as_str(), "as_str" | "as_ref")
        && expr_ty_is_adt_named(cx, recv, "Sql")
    }
    _ => false,
  }
}

/// The initializer of `let x = init;` when `id` is that immutable binding.
fn immutable_local_init<'tcx>(cx: &LateContext<'tcx>, id: HirId) -> Option<&'tcx Expr<'tcx>> {
  let Node::Pat(pat) = cx.tcx.hir_node(id) else {
    return None;
  };
  if !matches!(pat.kind, PatKind::Binding(BindingMode::NONE, _, _, None)) {
    return None;
  }
  match cx.tcx.parent_hir_node(id) {
    Node::LetStmt(local) if local.pat.hir_id == id && local.els.is_none() => local.init,
    _ => None,
  }
}

/// `true` when `id` is a parameter of the enclosing `fn` declared `&'static str`: the borrow
/// checker then holds every caller to a literal or a `const` (as `Sql` does).
fn is_static_str_param(cx: &LateContext<'_>, id: HirId) -> bool {
  let tcx = cx.tcx;
  let owner = tcx.hir_get_parent_item(id);
  let (Some(decl), Some(body)) = (
    tcx.hir_fn_decl_by_hir_id(tcx.local_def_id_to_hir_id(owner.def_id)),
    tcx.hir_maybe_body_owned_by(owner.def_id),
  ) else {
    return false;
  };
  // A closure's parameters are in the closure's body, not the fn's: they are not found here.
  body.params.iter().zip(decl.inputs).any(|(param, ty)| {
    param.pat.hir_id == id
      && matches!(
        ty.kind,
        rustc_hir::TyKind::Ref(lt, mut_ty)
          if lt.is_static() && ty_last_segment_is(mut_ty.ty, "str")
      )
  })
}

/// The literal inside `&"…".to_string()`, `&"…".to_owned()` or `&String::from("…")`.
fn wrapped_literal<'tcx>(expr: &'tcx Expr<'tcx>) -> Option<&'tcx Expr<'tcx>> {
  let is_str_lit =
    |e: &Expr<'_>| matches!(e.kind, ExprKind::Lit(lit) if matches!(lit.node, LitKind::Str(..)));
  let inner = match peel_refs(expr).kind {
    ExprKind::MethodCall(seg, recv, [], _)
      if matches!(seg.ident.name.as_str(), "to_string" | "to_owned") =>
    {
      recv
    }
    ExprKind::Call(callee, [arg]) => match &callee.kind {
      ExprKind::Path(QPath::TypeRelative(ty, seg))
        if seg.ident.name.as_str() == "from" && ty_last_segment_is(ty, "String") =>
      {
        arg
      }
      _ => return None,
    },
    _ => return None,
  };
  is_str_lit(inner).then_some(inner)
}
