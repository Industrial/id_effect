//! **Invariant** — `imap` for types that map in both directions with witnesses.

/// Invariant functor: map with encode/decode witnesses.
pub trait Invariant {
  /// Inner type.
  type Inner;
  /// Invariant map with forward `f` and backward witness `g`.
  fn imap<B>(
    self,
    f: impl FnOnce(Self::Inner) -> B,
    g: impl FnOnce(B) -> Self::Inner,
  ) -> Self::Output<B>
  where
    Self: Sized,
  {
    let _ = g;
    self.imap_f(f)
  }
  /// Forward-only invariant map.
  fn imap_f<B>(self, f: impl FnOnce(Self::Inner) -> B) -> Self::Output<B>;
  /// Result type after mapping.
  type Output<B>;
}

/// Plain newtype `imap`.
pub mod newtype {
  /// Apply `imap` to a plain value (discards backward witness at runtime).
  pub fn imap<A, B, F, G>(a: A, f: F, _g: G) -> B
  where
    F: FnOnce(A) -> B,
    G: FnOnce(B) -> A,
  {
    f(a)
  }
}

#[cfg(test)]
mod tests {
  use super::newtype::imap;

  #[test]
  fn imap_converts() {
    let s = imap(42i32, |n| n.to_string(), |s| s.parse().unwrap());
    assert_eq!(s, "42");
  }
}
