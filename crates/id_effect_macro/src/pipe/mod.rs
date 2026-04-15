//! [`pipe!`](macro@pipe) — left-to-right application (see [`Pipe`](::id_effect::Pipe)).

/// Apply functions left-to-right: `pipe!(x, f, g)` expands to `g(f(x))`.
#[macro_export]
macro_rules! pipe {
  ($x:expr) => {
    $x
  };
  ($x:expr, $f:expr $(, $rest:expr)*) => {
    $crate::pipe!($f($x) $(, $rest)*)
  };
}
