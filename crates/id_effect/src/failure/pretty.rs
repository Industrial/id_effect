//! Pretty-printing for [`Cause`] and [`Exit`].
//!
//! [`Cause::pretty`](super::cause::Cause::pretty) renders a compact single-line tree.
//! This module adds multi-line, indented rendering suitable for test output and logs.

use core::fmt::{Debug, Display, Write};

use super::cause::Cause;
use super::exit::Exit;
use crate::runtime::FiberId;

/// Indentation width for nested cause nodes.
const INDENT: &str = "  ";

/// Pretty-print a [`Cause`] as a multi-line indented tree.
pub fn pretty_cause<E>(cause: &Cause<E>) -> String
where
  E: Debug,
{
  let mut out = String::new();
  write_pretty_cause(cause, 0, &mut out);
  out
}

/// Pretty-print an [`Exit`] with a labelled success or failure branch.
pub fn pretty_exit<A, E>(exit: &Exit<A, E>) -> String
where
  A: Debug,
  E: Debug,
{
  match exit {
    Exit::Success(value) => format!("Success({value:?})"),
    Exit::Failure(cause) => format!("Failure(\n{}\n)", pretty_cause(cause)),
  }
}

fn write_pretty_cause<E>(cause: &Cause<E>, depth: usize, out: &mut String)
where
  E: Debug,
{
  let pad = INDENT.repeat(depth);
  match cause {
    Cause::Fail(error) => {
      let _ = writeln!(out, "{pad}Fail({error:?})");
    }
    Cause::Die(message) => {
      let _ = writeln!(out, "{pad}Die({message})");
    }
    Cause::Interrupt(fiber_id) => {
      let _ = writeln!(out, "{pad}Interrupt({fiber_id})");
    }
    Cause::Both(left, right) => {
      let _ = writeln!(out, "{pad}Both(");
      write_pretty_cause(left, depth + 1, out);
      write_pretty_cause(right, depth + 1, out);
      let _ = writeln!(out, "{pad})");
    }
    Cause::Then(left, right) => {
      let _ = writeln!(out, "{pad}Then(");
      write_pretty_cause(left, depth + 1, out);
      write_pretty_cause(right, depth + 1, out);
      let _ = writeln!(out, "{pad})");
    }
  }
}

/// Compact single-line rendering (delegates to [`Cause::pretty`]).
#[inline]
pub fn pretty_cause_inline<E>(cause: &Cause<E>) -> String
where
  E: Display + Clone + 'static,
{
  cause.pretty()
}

/// Format a fiber interrupt id for display-only contexts.
#[inline]
pub fn pretty_fiber_id(fiber_id: FiberId) -> String {
  format!("{fiber_id}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn pretty_cause_multiline_both() {
    let cause = Cause::both(Cause::fail("boom"), Cause::die("defect"));
    let rendered = pretty_cause(&cause);
    assert!(rendered.contains("Both("));
    assert!(rendered.contains("boom"));
    assert!(rendered.contains("Die(defect)"));
  }

  #[test]
  fn pretty_exit_success() {
    let exit = Exit::<i32, ()>::succeed(42);
    assert_eq!(pretty_exit(&exit), "Success(42)");
  }

  #[test]
  fn pretty_exit_failure_includes_cause_tree() {
    let exit = Exit::<(), &str>::fail("err");
    let rendered = pretty_exit(&exit);
    assert!(rendered.starts_with("Failure("));
    assert!(rendered.contains("err"));
  }

  #[test]
  fn pretty_cause_inline_matches_cause_pretty() {
    let cause = Cause::then(Cause::fail("a"), Cause::die("b"));
    assert_eq!(pretty_cause_inline(&cause), cause.pretty());
  }
}
