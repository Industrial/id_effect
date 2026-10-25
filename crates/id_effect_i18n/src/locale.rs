//! [`Locale`]: a validated, case-normalized language identifier.

use std::fmt;

use unic_langid::LanguageIdentifier;

/// A validated language identifier: `language[-Script][-REGION]`.
///
/// The language subtag is 2–3 ASCII letters (stored lowercase), the optional script subtag 4 ASCII
/// letters (stored titlecase) and the optional region subtag 2 ASCII letters (stored uppercase) or
/// 3 ASCII digits. Subtags are separated by `-` or `_`; the canonical form uses `-`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Locale {
  tag: String,
  // Byte offsets into `tag`, which is at most `MAX_LEN` bytes long.
  language_len: u8,
  script: Option<(u8, u8)>,
  region: Option<(u8, u8)>,
}

/// The `start..end` subtag of `tag`.
fn subtag(tag: &str, (start, end): (u8, u8)) -> &str {
  &tag[usize::from(start)..usize::from(end)]
}

/// Why [`Locale::parse`] rejected its input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocaleError {
  /// The input was empty.
  Empty,
  /// The input was longer than [`Locale::MAX_LEN`] bytes.
  TooLong,
  /// The language subtag is not 2–3 ASCII letters.
  InvalidLanguage,
  /// A subtag after the language is neither a script (4 letters) nor a region (2 letters or 3
  /// digits), or appears out of order.
  InvalidSubtag,
}

impl fmt::Display for LocaleError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::Empty => "locale is empty",
      Self::TooLong => "locale is longer than the limit",
      Self::InvalidLanguage => "locale language subtag must be 2-3 ASCII letters",
      Self::InvalidSubtag => "locale subtag is not a script or region",
    })
  }
}

impl std::error::Error for LocaleError {}

impl Locale {
  /// The longest input [`Locale::parse`] reads, in bytes.
  pub const MAX_LEN: usize = 16;

  /// Parses and normalizes a language identifier such as `en`, `de-AT`, `sr_Latn_RS` or `es-419`.
  ///
  /// # Errors
  ///
  /// Returns a [`LocaleError`] when the input is empty, longer than [`Locale::MAX_LEN`] bytes, or
  /// not of the shape `language[-Script][-REGION]`.
  pub fn parse(input: &str) -> Result<Self, LocaleError> {
    if input.is_empty() {
      return Err(LocaleError::Empty);
    }
    if input.len() > Self::MAX_LEN {
      return Err(LocaleError::TooLong);
    }
    let mut parts = input.split(['-', '_']);
    let language = parts.next().unwrap_or_default();
    if !(2..=3).contains(&language.len()) || !language.bytes().all(|b| b.is_ascii_alphabetic()) {
      return Err(LocaleError::InvalidLanguage);
    }
    // The canonical tag is as long as the input (at most `MAX_LEN` bytes), so offsets fit a u8.
    let mut tag = language.to_ascii_lowercase();
    let offset = |tag: &str| tag.len() as u8;
    let language_len = offset(&tag);
    let mut script = None;
    let mut region = None;
    for part in parts {
      let start = offset(&tag) + 1;
      if script.is_none() && region.is_none() && is_script(part) {
        tag.push('-');
        tag.push_str(&part[..1].to_ascii_uppercase());
        tag.push_str(&part[1..].to_ascii_lowercase());
        script = Some((start, offset(&tag)));
      } else if region.is_none() && is_region(part) {
        tag.push('-');
        tag.push_str(&part.to_ascii_uppercase());
        region = Some((start, offset(&tag)));
      } else {
        return Err(LocaleError::InvalidSubtag);
      }
    }
    Ok(Self {
      tag,
      language_len,
      script,
      region,
    })
  }

  /// The canonical form, for example `de-AT`.
  pub fn as_str(&self) -> &str {
    &self.tag
  }

  /// The language subtag, lowercase (`de` for `de-AT`).
  pub fn language(&self) -> &str {
    subtag(&self.tag, (0, self.language_len))
  }

  /// The script subtag, titlecase (`Latn` for `sr-Latn-RS`), if present.
  pub fn script(&self) -> Option<&str> {
    self.script.map(|range| subtag(&self.tag, range))
  }

  /// The region subtag, uppercase or digits (`AT` for `de-AT`, `419` for `es-419`), if present.
  pub fn region(&self) -> Option<&str> {
    self.region.map(|range| subtag(&self.tag, range))
  }

  /// The locale with only its language subtag (`de` for `de-AT`): the next step of a fallback
  /// chain.
  pub fn language_only(&self) -> Self {
    Self {
      tag: self.language().to_owned(),
      language_len: self.language_len,
      script: None,
      region: None,
    }
  }

  /// Whether this locale is only a language subtag (`de`, not `de-AT`).
  pub fn is_language_only(&self) -> bool {
    self.script.is_none() && self.region.is_none()
  }
}

impl fmt::Display for Locale {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.tag)
  }
}

impl std::str::FromStr for Locale {
  type Err = LocaleError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Self::parse(s)
  }
}

/// The Unicode language identifier Fluent and `fluent-langneg` work with.
impl From<&Locale> for LanguageIdentifier {
  fn from(locale: &Locale) -> Self {
    // Every `Locale` is a valid identifier (a subset of the BCP 47 shapes unic-langid accepts).
    locale.as_str().parse().unwrap_or_default()
  }
}

/// A [`Locale`] from a Unicode language identifier; identifiers with variants
/// (`de-DE-1901`) or longer than [`Locale::MAX_LEN`] are rejected.
impl TryFrom<&LanguageIdentifier> for Locale {
  type Error = LocaleError;

  fn try_from(id: &LanguageIdentifier) -> Result<Self, Self::Error> {
    Self::parse(&id.to_string())
  }
}

fn is_script(part: &str) -> bool {
  part.len() == 4 && part.bytes().all(|b| b.is_ascii_alphabetic())
}

fn is_region(part: &str) -> bool {
  (part.len() == 2 && part.bytes().all(|b| b.is_ascii_alphabetic()))
    || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn locale_parse_normalizes_case_and_separator() {
    let locale = Locale::parse("SR_latn_rs").unwrap();
    assert_eq!(locale.as_str(), "sr-Latn-RS");
    assert_eq!(locale.language(), "sr");
    assert_eq!(locale.script(), Some("Latn"));
    assert_eq!(locale.region(), Some("RS"));
    assert_eq!(locale.to_string(), "sr-Latn-RS");
    assert_eq!(locale.language_only().as_str(), "sr");
    assert!(!locale.is_language_only());
    assert!(locale.language_only().is_language_only());
  }

  #[test]
  fn locale_parse_accepts_numeric_region_and_language_only() {
    let es = "es-419".parse::<Locale>().unwrap();
    assert_eq!(es.region(), Some("419"));
    assert_eq!(es.script(), None);
    let en = Locale::parse("en").unwrap();
    assert_eq!((en.script(), en.region()), (None, None));
    assert_eq!(en.language_only(), en);
  }

  #[test]
  fn r4_locale_parse_rejects_malformed_and_oversize_input() {
    assert_eq!(Locale::parse(""), Err(LocaleError::Empty));
    assert_eq!(Locale::parse(&"a".repeat(17)), Err(LocaleError::TooLong));
    assert_eq!(Locale::parse("e"), Err(LocaleError::InvalidLanguage));
    assert_eq!(Locale::parse("e1"), Err(LocaleError::InvalidLanguage));
    assert_eq!(Locale::parse("../etc"), Err(LocaleError::InvalidLanguage));
    assert_eq!(Locale::parse("de-AT-Latn"), Err(LocaleError::InvalidSubtag));
    assert_eq!(Locale::parse("de-"), Err(LocaleError::InvalidSubtag));
    assert_eq!(Locale::parse("de-AT-DE"), Err(LocaleError::InvalidSubtag));
    for err in [
      LocaleError::Empty,
      LocaleError::TooLong,
      LocaleError::InvalidLanguage,
      LocaleError::InvalidSubtag,
    ] {
      assert!(!err.to_string().is_empty());
    }
  }

  #[test]
  fn locale_converts_to_and_from_language_identifier() {
    for tag in ["en", "de-AT", "sr-Latn-RS", "es-419"] {
      let locale = Locale::parse(tag).unwrap();
      let id = LanguageIdentifier::from(&locale);
      assert_eq!(id.to_string(), tag);
      assert_eq!(Locale::try_from(&id), Ok(locale));
    }
    let with_variant: LanguageIdentifier = "de-DE-1901".parse().unwrap();
    assert_eq!(
      Locale::try_from(&with_variant),
      Err(LocaleError::InvalidSubtag)
    );
  }
}
