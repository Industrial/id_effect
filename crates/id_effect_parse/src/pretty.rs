//! Wadler-style pretty documents and the [`Pretty`] trait.

use core::fmt;

/// A pretty-printing document (lazy layout resolved at render time).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doc {
  /// Empty document.
  Nil,
  /// Literal text (must not contain newlines).
  Text(String),
  /// Hard line break.
  Line,
  /// Soft break (space when flat, newline when broken).
  Break,
  /// Concatenate two documents.
  Cat(Box<Doc>, Box<Doc>),
  /// Increase indentation for the nested document.
  Nest(usize, Box<Doc>),
  /// Group for choosing flat vs broken layout.
  Group(Box<Doc>),
}

impl Doc {
  /// Empty document.
  #[must_use]
  pub fn nil() -> Self {
    Self::Nil
  }

  /// Literal text.
  #[must_use]
  pub fn text(text: impl Into<String>) -> Self {
    Self::Text(text.into())
  }

  /// Hard line break.
  #[must_use]
  pub fn line() -> Self {
    Self::Line
  }

  /// Soft break.
  #[must_use]
  pub fn break_() -> Self {
    Self::Break
  }

  /// Concatenate documents left-to-right.
  #[must_use]
  pub fn cat(self, other: Doc) -> Self {
    Self::Cat(Box::new(self), Box::new(other))
  }

  /// Nest with the given indentation width (spaces).
  #[must_use]
  pub fn nest(self, indent: usize) -> Self {
    Self::Nest(indent, Box::new(self))
  }

  /// Mark a subtree for flat/broken layout selection.
  #[must_use]
  pub fn group(self) -> Self {
    Self::Group(Box::new(self))
  }

  /// Render to a string using `width` as the line budget.
  #[must_use]
  pub fn render(&self, width: usize) -> String {
    let mut out = String::new();
    self.render_impl(width, 0, Mode::Broken, &mut out);
    out
  }

  fn render_impl(&self, width: usize, indent: usize, mode: Mode, out: &mut String) {
    match self {
      Doc::Nil => {}
      Doc::Text(text) => out.push_str(text),
      Doc::Line => {
        out.push('\n');
        out.push_str(&" ".repeat(indent));
      }
      Doc::Break => match mode {
        Mode::Flat => out.push(' '),
        Mode::Broken => {
          out.push('\n');
          out.push_str(&" ".repeat(indent));
        }
      },
      Doc::Cat(left, right) => {
        left.render_impl(width, indent, mode, out);
        right.render_impl(width, indent, mode, out);
      }
      Doc::Nest(spaces, inner) => inner.render_impl(width, indent + spaces, mode, out),
      Doc::Group(inner) => {
        let mut flat = String::new();
        inner.render_impl(usize::MAX, indent, Mode::Flat, &mut flat);
        if flat.contains('\n') || flat.len() > width {
          inner.render_impl(width, indent, Mode::Broken, out);
        } else {
          out.push_str(&flat);
        }
      }
    }
  }

  /// Fill a line with repeated documents separated by soft breaks.
  #[must_use]
  pub fn fill(docs: impl IntoIterator<Item = Doc>) -> Doc {
    let mut iter = docs.into_iter();
    let Some(first) = iter.next() else {
      return Self::nil();
    };
    let mut doc = first;
    for next in iter {
      doc = doc.cat(Doc::break_()).cat(next);
    }
    doc.group()
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
  Flat,
  Broken,
}

/// Values that can be turned into a [`Doc`].
pub trait Pretty {
  /// Build a pretty document for this value.
  fn pretty(&self) -> Doc;
}

impl Pretty for str {
  fn pretty(&self) -> Doc {
    Doc::text(self.to_string())
  }
}

impl Pretty for String {
  fn pretty(&self) -> Doc {
    Doc::text(self.clone())
  }
}

impl Pretty for i64 {
  fn pretty(&self) -> Doc {
    Doc::text(self.to_string())
  }
}

impl<T: Pretty> Pretty for [T] {
  fn pretty(&self) -> Doc {
    let mut doc = Doc::text("[");
    for (idx, item) in self.iter().enumerate() {
      if idx > 0 {
        doc = doc.cat(Doc::text(", ")).cat(item.pretty());
      } else {
        doc = doc.cat(item.pretty());
      }
    }
    doc.cat(Doc::text("]"))
  }
}

impl fmt::Display for Doc {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.render(80))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn doc_render_variants() {
    let doc = Doc::text("hi")
      .cat(Doc::line())
      .cat(Doc::text("there"))
      .nest(2)
      .group();
    let rendered = doc.render(80);
    assert!(rendered.contains("hi"));
    assert!(rendered.contains("there"));
  }

  #[test]
  fn pretty_string_type() {
    let s = String::from("hello");
    assert_eq!(s.pretty().render(40), "hello");
  }

  #[test]
  fn pretty_trait_for_primitives() {
    assert_eq!(42i64.pretty().render(40), "42");
    assert_eq!("x".pretty().render(40), "x");
    assert_eq!("[1, 2]".pretty().render(40), "[1, 2]");
  }

  #[test]
  fn doc_group_flat_when_fits() {
    let doc = Doc::group(Doc::text("short"));
    assert_eq!(doc.render(80), "short");
    assert_eq!(Doc::nil().render(10), "");
    assert_eq!(
      Doc::line().render(10),
      "
"
    );
  }

  #[test]
  fn display_uses_render() {
    let doc = Doc::text("ok").cat(Doc::break_());
    assert!(format!("{doc}").contains("ok"));
  }
}
