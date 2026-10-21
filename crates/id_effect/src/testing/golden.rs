//! Golden-file (snapshot) assertions: compare rendered output with a checked-in file, and
//! rewrite the file when [`BLESS_ENV`] is set.
//!
//! - A relative path resolves against `CARGO_MANIFEST_DIR` (the crate under test), so
//!   `tests/golden/report.txt` works from unit and integration tests alike.
//! - Line endings are normalized to `\n` on both sides before comparing.
//! - `ID_EFFECT_BLESS=1 cargo test` writes the actual output (creating directories) instead of
//!   comparing; review the diff and commit it. A missing golden file fails with that hint.
//! - [`render_exit`] renders an [`Exit`] without its run-specific parts (fiber ids), so an
//!   interrupted exit is stable across runs.

use std::fmt::{Debug, Write as _};
use std::path::{Path, PathBuf};

use crate::{Cause, Exit};

/// The environment variable that turns golden assertions into writes (`ID_EFFECT_BLESS=1`).
pub const BLESS_ENV: &str = "ID_EFFECT_BLESS";

/// Asserts `actual` equals the golden file at `path` (or writes it when [`BLESS_ENV`] is set).
///
/// # Panics
///
/// If the file is missing or differs (the message holds a line diff), or if blessing cannot
/// write the file.
///
/// ```no_run
/// use id_effect::assert_golden;
///
/// assert_golden("tests/golden/greeting.txt", "hello\n");
/// ```
#[track_caller]
pub fn assert_golden(path: impl AsRef<Path>, actual: &str) {
  let path = golden_path(path.as_ref());
  let actual = normalize(actual);
  if blessing() {
    if let Some(dir) = path.parent() {
      std::fs::create_dir_all(dir)
        .unwrap_or_else(|e| panic!("golden: cannot create {}: {e}", dir.display()));
    }
    std::fs::write(&path, actual.as_bytes())
      .unwrap_or_else(|e| panic!("golden: cannot write {}: {e}", path.display()));
    return;
  }
  let expected = match std::fs::read_to_string(&path) {
    Ok(s) => normalize(&s),
    Err(e) => panic!(
      "golden: cannot read {} ({e}); run with {BLESS_ENV}=1 to create it",
      path.display()
    ),
  };
  if expected != actual {
    panic!(
      "golden mismatch: {}\n{}\nrun with {BLESS_ENV}=1 to accept the new output",
      path.display(),
      line_diff(&expected, &actual)
    );
  }
}

/// [`assert_golden`] on the pretty `Debug` rendering (`{:#?}`) of `value`, plus a final newline.
#[track_caller]
pub fn assert_golden_debug<T: Debug + ?Sized>(path: impl AsRef<Path>, value: &T) {
  assert_golden(path, &format!("{value:#?}\n"));
}

/// [`assert_golden`] on [`render_exit`] of `exit`, plus a final newline.
#[track_caller]
pub fn assert_golden_exit<A: Debug, E: Debug>(path: impl AsRef<Path>, exit: &Exit<A, E>) {
  assert_golden(path, &format!("{}\n", render_exit(exit)));
}

/// A stable rendering of `exit`: `Success(<value:#?>)` or `Failure(<cause>)`, where a cause is
/// `Fail(<e:?>)`, `Die("<message>")`, `Interrupt` (no fiber id), `Both(l, r)` or `Then(l, r)`.
///
/// ```
/// use id_effect::{Cause, Exit, FiberId, render_exit};
///
/// let exit: Exit<u8, &str> = Exit::Failure(Cause::both(
///   Cause::fail("boom"),
///   Cause::interrupt(FiberId::new(42)),
/// ));
/// assert_eq!(render_exit(&exit), r#"Failure(Both(Fail("boom"), Interrupt))"#);
/// ```
pub fn render_exit<A: Debug, E: Debug>(exit: &Exit<A, E>) -> String {
  match exit {
    Exit::Success(value) => format!("Success({value:#?})"),
    Exit::Failure(cause) => {
      let mut out = String::from("Failure(");
      render_cause(cause, &mut out);
      out.push(')');
      out
    }
  }
}

fn render_cause<E: Debug>(cause: &Cause<E>, out: &mut String) {
  match cause {
    Cause::Fail(e) => {
      let _ = write!(out, "Fail({e:?})");
    }
    Cause::Die(d) => {
      let _ = write!(out, "Die({:?})", d.message());
    }
    Cause::Interrupt(_) => out.push_str("Interrupt"),
    Cause::Both(l, r) | Cause::Then(l, r) => {
      out.push_str(if matches!(cause, Cause::Both(..)) {
        "Both("
      } else {
        "Then("
      });
      render_cause(l, out);
      out.push_str(", ");
      render_cause(r, out);
      out.push(')');
    }
  }
}

fn blessing() -> bool {
  std::env::var_os(BLESS_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

fn golden_path(path: &Path) -> PathBuf {
  if path.is_absolute() {
    return path.to_path_buf();
  }
  match std::env::var_os("CARGO_MANIFEST_DIR") {
    Some(dir) => Path::new(&dir).join(path),
    None => path.to_path_buf(),
  }
}

fn normalize(s: &str) -> String {
  s.replace("\r\n", "\n")
}

/// Line-by-line comparison: `-` expected, `+` actual, for every differing line (at most 40).
fn line_diff(expected: &str, actual: &str) -> String {
  let exp: Vec<&str> = expected.lines().collect();
  let act: Vec<&str> = actual.lines().collect();
  let mut out = String::new();
  let mut shown = 0;
  for i in 0..exp.len().max(act.len()) {
    let (e, a) = (exp.get(i), act.get(i));
    if e == a {
      continue;
    }
    if shown == 40 {
      out.push_str("  … more differing lines\n");
      break;
    }
    shown += 1;
    let _ = writeln!(out, "  line {}:", i + 1);
    if let Some(e) = e {
      let _ = writeln!(out, "  - {e}");
    }
    if let Some(a) = a {
      let _ = writeln!(out, "  + {a}");
    }
  }
  if shown == 0 {
    out.push_str("  (the texts differ only in trailing newlines)\n");
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::FiberId;

  fn temp_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("id_effect_golden_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
  }

  #[test]
  fn matching_file_passes_with_crlf_normalized() {
    let path = temp_file("match.txt");
    std::fs::write(&path, "a\r\nb\r\n").unwrap();
    assert_golden(&path, "a\nb\n");
  }

  #[test]
  #[should_panic(expected = "golden mismatch")]
  fn differing_file_fails() {
    let path = temp_file("differ.txt");
    std::fs::write(&path, "a\nb\n").unwrap();
    assert_golden(&path, "a\nc\n");
  }

  #[test]
  #[should_panic(expected = "ID_EFFECT_BLESS=1 to create it")]
  fn missing_file_fails_with_hint() {
    assert_golden(temp_file("missing-never-written.txt"), "x");
  }

  #[test]
  fn line_diff_lists_changed_lines() {
    let d = line_diff("a\nb\nc", "a\nB\nc\nd");
    assert!(d.contains("line 2:\n  - b\n  + B"), "{d}");
    assert!(d.contains("line 4:\n  + d"), "{d}");
    assert!(line_diff("a\n", "a").contains("trailing newlines"));
  }

  #[test]
  fn relative_paths_resolve_against_the_manifest_dir() {
    let p = golden_path(Path::new("tests/golden/x.txt"));
    assert!(p.ends_with("crates/id_effect/tests/golden/x.txt"), "{p:?}");
    // An absolute path on this platform (`/abs/x` is relative to the drive on Windows).
    let abs = std::env::temp_dir().join("x");
    assert_eq!(golden_path(&abs), abs);
  }

  #[test]
  fn render_exit_is_stable() {
    assert_eq!(render_exit(&Exit::<u8, ()>::succeed(3)), "Success(3)");
    let exit: Exit<(), String> = Exit::Failure(Cause::then(
      Cause::die("bad"),
      Cause::both(Cause::fail("e".into()), Cause::interrupt(FiberId::new(9))),
    ));
    assert_eq!(
      render_exit(&exit),
      r#"Failure(Then(Die("bad"), Both(Fail("e"), Interrupt)))"#
    );
  }

  #[test]
  fn golden_debug_and_exit_render_with_newline() {
    let path = temp_file("debug.txt");
    std::fs::write(&path, "[\n    1,\n    2,\n]\n").unwrap();
    assert_golden_debug(&path, &[1, 2]);
    let path = temp_file("exit.txt");
    std::fs::write(&path, "Failure(Fail(7))\n").unwrap();
    assert_golden_exit(&path, &Exit::<(), u8>::fail(7));
  }
}
