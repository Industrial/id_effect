//! [`Title`]: the validated title of a todo.

use std::fmt;

use id_effect::EffectData;

use super::InvalidTitle;

/// A title: trimmed, not empty, at most [`Title::MAX_CHARS`] characters. Titles are unique
/// across todos (the store enforces it: [`TitleTaken`](super::TitleTaken)).
#[derive(Clone, Debug, EffectData)]
pub struct Title(String);

impl Title {
  /// The longest accepted title, in characters.
  pub const MAX_CHARS: usize = 200;

  /// Validates `raw`: trimmed, then rejected when empty or longer than [`Self::MAX_CHARS`].
  pub fn parse(raw: &str) -> Result<Self, InvalidTitle> {
    let title = raw.trim();
    if title.is_empty() {
      return Err(InvalidTitle::Empty);
    }
    if title.chars().count() > Self::MAX_CHARS {
      return Err(InvalidTitle::TooLong {
        max: Self::MAX_CHARS,
      });
    }
    Ok(Self(title.to_owned()))
  }

  /// The title text.
  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// The title text, owned.
  pub fn into_inner(self) -> String {
    self.0
  }
}

impl fmt::Display for Title {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn trims_and_validates() {
    assert_eq!(Title::parse(" milk ").unwrap().as_str(), "milk");
    assert_eq!(Title::parse("   "), Err(InvalidTitle::Empty));
    let long = "x".repeat(Title::MAX_CHARS + 1);
    assert_eq!(
      Title::parse(&long),
      Err(InvalidTitle::TooLong {
        max: Title::MAX_CHARS
      })
    );
    assert!(Title::parse(&"é".repeat(Title::MAX_CHARS)).is_ok());
  }

  #[test]
  fn displays_its_text() {
    let title = Title::parse("eggs").unwrap();
    assert_eq!(title.to_string(), "eggs");
    assert_eq!(title.into_inner(), "eggs");
    assert_eq!(
      InvalidTitle::TooLong { max: 3 }.to_string(),
      "the title must be at most 3 characters"
    );
  }
}
