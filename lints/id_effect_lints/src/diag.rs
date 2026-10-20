/// Thin wrappers around `LintContext::emit_span_lint` so the rest of the crate
/// doesn't need to import `rustc_errors` directly.
use rustc_errors::{Applicability, Diag, DiagDecorator, DiagMessage};
use rustc_lint::{LateContext, Lint, LintContext};
use rustc_span::Span;

/// Emit a lint at `span` with an optional help note.
pub fn span_lint_and_help(
  cx: &LateContext<'_>,
  lint: &'static Lint,
  span: Span,
  msg: impl Into<DiagMessage>,
  _help_span: Option<Span>,
  help: impl Into<DiagMessage>,
) {
  let msg = msg.into();
  let help = help.into();
  cx.emit_span_lint(
    lint,
    span,
    DiagDecorator(move |diag: &mut Diag<'_, ()>| {
      diag.primary_message(msg.clone());
      diag.help(help.clone());
    }),
  );
}

/// Emit a lint at `span` with one MachineApplicable replacement per `(span, text)` part
/// (`cargo dylint --fix` applies them).
pub fn span_lint_and_sugg(
  cx: &LateContext<'_>,
  lint: &'static Lint,
  span: Span,
  msg: impl Into<DiagMessage>,
  help: impl Into<DiagMessage>,
  parts: Vec<(Span, String)>,
) {
  let msg = msg.into();
  let help = help.into();
  cx.emit_span_lint(
    lint,
    span,
    DiagDecorator(move |diag: &mut Diag<'_, ()>| {
      diag.primary_message(msg.clone());
      diag.multipart_suggestion(
        help.clone(),
        parts.clone(),
        Applicability::MachineApplicable,
      );
    }),
  );
}

/// Emit a plain lint at `span` (available for lints that do not need a help line).
#[allow(dead_code)]
pub fn span_lint(
  cx: &LateContext<'_>,
  lint: &'static Lint,
  span: Span,
  msg: impl Into<DiagMessage>,
) {
  let msg = msg.into();
  cx.emit_span_lint(
    lint,
    span,
    DiagDecorator(move |diag: &mut Diag<'_, ()>| {
      diag.primary_message(msg.clone());
    }),
  );
}
