//! [`Optional`] — lens-like focus into `Option<T>` with fallible get/set.

use crate::lens::Lens;

/// Focus into an `Option<T>` field with total outer `S`.
#[derive(Clone)]
pub struct Optional<S, T> {
  lens: Lens<S, Option<T>>,
}

impl<S: 'static, T: 'static> Optional<S, T> {
  /// Wrap a lens whose focus is `Option<T>`.
  pub fn new(lens: Lens<S, Option<T>>) -> Self {
    Self { lens }
  }

  /// Read the inner value when present.
  pub fn get(&self, source: &S) -> Option<T>
  where
    T: Clone,
  {
    self.lens.get(source).clone()
  }

  /// Set the optional field to `Some(value)`.
  pub fn set_some(&self, source: S, value: T) -> S {
    self.lens.set(source, Some(value))
  }

  /// Set the optional field to `None`.
  pub fn set_none(&self, source: S) -> S {
    self.lens.set(source, None)
  }

  /// Modify the inner value when present; leave `S` unchanged when absent.
  pub fn modify(&self, source: S, f: impl FnOnce(T) -> T) -> S
  where
    T: Clone,
  {
    self.lens.modify(source, |opt| opt.map(f))
  }

  /// Replace the whole `Option<T>` field.
  pub fn set(&self, source: S, value: Option<T>) -> S {
    self.lens.set(source, value)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::lens::field;

  #[derive(Clone, Debug, PartialEq)]
  struct Profile {
    nickname: Option<String>,
  }

  fn nickname_opt() -> Optional<Profile, String> {
    Optional::new(field(
      |p: &Profile| &p.nickname,
      |mut p, nickname| {
        p.nickname = nickname;
        p
      },
    ))
  }

  mod get {
    use super::*;

    #[test]
    fn returns_some_when_present() {
      let profile = Profile {
        nickname: Some("ada".into()),
      };
      assert_eq!(nickname_opt().get(&profile), Some("ada".into()));
    }

    #[test]
    fn returns_none_when_absent() {
      let profile = Profile { nickname: None };
      assert_eq!(nickname_opt().get(&profile), None);
    }
  }

  mod set_some {
    use super::*;

    #[test]
    fn inserts_value() {
      let profile = Profile { nickname: None };
      let updated = nickname_opt().set_some(profile, "grace".into());
      assert_eq!(updated.nickname, Some("grace".into()));
    }
  }

  mod set_none {
    use super::*;

    #[test]
    fn clears_value() {
      let profile = Profile {
        nickname: Some("ada".into()),
      };
      let updated = nickname_opt().set_none(profile);
      assert_eq!(updated.nickname, None);
    }
  }

  mod modify {
    use super::*;

    #[test]
    fn transforms_present_value() {
      let profile = Profile {
        nickname: Some("ada".into()),
      };
      let updated = nickname_opt().modify(profile, |n| n.to_uppercase());
      assert_eq!(updated.nickname, Some("ADA".into()));
    }

    #[test]
    fn leaves_absent_value_unchanged() {
      let profile = Profile { nickname: None };
      let updated = nickname_opt().modify(profile, |n| n.to_uppercase());
      assert_eq!(updated.nickname, None);
    }
  }
}
