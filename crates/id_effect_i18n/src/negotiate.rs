//! Choosing the locale of a request: a bounded `Accept-Language` parser
//! ([`parse_accept_language`]), negotiation against the supported-locale allowlist
//! ([`Negotiator`], over `fluent-langneg`) and the [`Localizer`] the choice produces.
//!
//! A request names its language in up to three places, tried in this order by
//! [`Negotiator::select`]:
//!
//! 1. an application **preference** (a stored user setting, for example),
//! 2. a **cookie** set by a language switcher,
//! 3. the **`Accept-Language`** header the browser sends.
//!
//! Each is untrusted input. A value is used only when it matches a supported locale; otherwise the
//! next source is tried, and the default locale is the last resort, so the chosen locale is always
//! one of [`Negotiator::supported`].
//!
//! ```
//! use id_effect_i18n::Locale;
//! use id_effect_i18n::negotiate::{LocaleSource, Negotiator};
//!
//! let en = Locale::parse("en").unwrap();
//! let de = Locale::parse("de").unwrap();
//! let negotiator = Negotiator::new(en.clone(), [de.clone()]);
//!
//! // `fr` is not supported; German is the best the client accepts.
//! let header = Some("fr-CH, fr;q=0.9, de-AT;q=0.8, en;q=0.5");
//! assert_eq!(negotiator.select(None, None, header), (de.clone(), LocaleSource::AcceptLanguage));
//! // A cookie with a supported locale beats the header.
//! assert_eq!(negotiator.select(None, Some("en"), header), (en.clone(), LocaleSource::Cookie));
//! // Nothing usable: the default.
//! assert_eq!(negotiator.select(Some("xx"), Some("../etc"), None), (en, LocaleSource::Default));
//! ```
//!
//! ## Secure by default (ADR 0013 R4)
//!
//! [`parse_accept_language`] reads at most [`MAX_ACCEPT_LANGUAGE_BYTES`] bytes and
//! [`MAX_ACCEPT_LANGUAGE_RANGES`] ranges; a longer header is ignored as a whole (the request gets
//! the default locale), never truncated or partially parsed. Language tags go through
//! [`Locale::parse`] (at most [`Locale::MAX_LEN`] bytes of ASCII subtags), and a locale outside the
//! allowlist is never selected, so no header value reaches a catalog lookup or a file path.

use std::fmt;

use fluent_langneg::NegotiationStrategy;
use fluent_langneg::negotiate::filter_matches;
use id_effect::{Effect, Selector};
use unic_langid::LanguageIdentifier;

use crate::{Args, Catalog, CurrentLocale, Locale, with_locale};

/// The longest `Accept-Language` value [`parse_accept_language`] reads, in bytes; a longer value
/// is ignored.
pub const MAX_ACCEPT_LANGUAGE_BYTES: usize = 256;

/// The most language ranges [`parse_accept_language`] reads; a value with more is ignored.
pub const MAX_ACCEPT_LANGUAGE_RANGES: usize = 32;

/// Why [`parse_accept_language`] ignored a header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptLanguageError {
  /// The value is longer than [`MAX_ACCEPT_LANGUAGE_BYTES`].
  TooLong,
  /// The value has more than [`MAX_ACCEPT_LANGUAGE_RANGES`] ranges.
  TooManyRanges,
}

impl fmt::Display for AcceptLanguageError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(match self {
      Self::TooLong => "Accept-Language is longer than the limit",
      Self::TooManyRanges => "Accept-Language has more ranges than the limit",
    })
  }
}

impl std::error::Error for AcceptLanguageError {}

/// One language range of an `Accept-Language` header: a locale (or `*`) and its weight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageRange {
  locale: Option<Locale>,
  quality: u16,
}

impl LanguageRange {
  /// The range's locale; `None` for the wildcard `*`.
  pub fn locale(&self) -> Option<&Locale> {
    self.locale.as_ref()
  }

  /// Whether this is the wildcard range `*`.
  pub fn is_wildcard(&self) -> bool {
    self.locale.is_none()
  }

  /// The weight in thousandths: `q=0.8` is `800`, no `q` is `1000`. Never `0` (ranges with
  /// `q=0`, "not acceptable", are dropped by the parser).
  pub fn quality(&self) -> u16 {
    self.quality
  }
}

/// Parses an `Accept-Language` value (RFC 9110 §12.5.4) into its ranges, best first.
///
/// Ranges are sorted by weight, highest first; ranges of equal weight keep the client's order.
/// The parser is lenient inside the limits: a range whose tag is not a [`Locale`] is reduced to
/// its language subtag when that is valid (`en-US-x-twain` → `en`) and dropped otherwise, a range
/// with a malformed or zero `q` is dropped, and other parameters are ignored.
///
/// This is the fuzzing entry point for the header (target `i18n_accept_language`).
///
/// ```
/// use id_effect_i18n::negotiate::{AcceptLanguageError, parse_accept_language};
///
/// let ranges = parse_accept_language("en;q=0.5, de-AT, *;q=0.1").unwrap();
/// let tags: Vec<_> = ranges.iter().map(|r| r.locale().map(|l| l.as_str())).collect();
/// assert_eq!(tags, [Some("de-AT"), Some("en"), None]);
/// assert_eq!(ranges[1].quality(), 500);
///
/// let flood = vec!["en"; 33].join(",");
/// assert_eq!(parse_accept_language(&flood), Err(AcceptLanguageError::TooManyRanges));
/// ```
///
/// # Errors
///
/// [`AcceptLanguageError::TooLong`] above [`MAX_ACCEPT_LANGUAGE_BYTES`] bytes and
/// [`AcceptLanguageError::TooManyRanges`] above [`MAX_ACCEPT_LANGUAGE_RANGES`] ranges: the whole
/// header is then to be ignored.
pub fn parse_accept_language(header: &str) -> Result<Vec<LanguageRange>, AcceptLanguageError> {
  if header.len() > MAX_ACCEPT_LANGUAGE_BYTES {
    return Err(AcceptLanguageError::TooLong);
  }
  let mut ranges = Vec::new();
  let mut seen = 0_usize;
  for item in header.split(',') {
    let item = item.trim_matches([' ', '\t']);
    if item.is_empty() {
      continue;
    }
    seen += 1;
    if seen > MAX_ACCEPT_LANGUAGE_RANGES {
      return Err(AcceptLanguageError::TooManyRanges);
    }
    if let Some(range) = parse_range(item) {
      ranges.push(range);
    }
  }
  // Stable: equal weights keep the client's order.
  ranges.sort_by_key(|range| std::cmp::Reverse(range.quality));
  Ok(ranges)
}

/// One `tag[;q=w][;other]` item, or `None` when it is dropped.
fn parse_range(item: &str) -> Option<LanguageRange> {
  let mut parts = item.split(';');
  let tag = parts.next().unwrap_or_default().trim_matches([' ', '\t']);
  let mut quality = 1000;
  for param in parts {
    let param = param.trim_matches([' ', '\t']);
    if let Some(value) = param
      .strip_prefix("q=")
      .or_else(|| param.strip_prefix("Q="))
    {
      quality = parse_quality(value)?;
    }
  }
  if quality == 0 {
    return None;
  }
  let locale = if tag == "*" {
    None
  } else {
    Some(range_locale(tag)?)
  };
  Some(LanguageRange { locale, quality })
}

/// An RFC 9110 qvalue (`0`, `0.8`, `0.125`, `1`, `1.000`) in thousandths.
fn parse_quality(value: &str) -> Option<u16> {
  let (int, frac) = value.split_once('.').unwrap_or((value, ""));
  if frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
    return None;
  }
  let thousandths = frac
    .bytes()
    .zip([100_u16, 10, 1])
    .map(|(digit, scale)| u16::from(digit - b'0') * scale)
    .sum::<u16>();
  match int {
    "0" => Some(thousandths),
    "1" if thousandths == 0 => Some(1000),
    _ => None,
  }
}

/// The locale of a range tag, or of its language subtag when the full tag is not a [`Locale`].
fn range_locale(tag: &str) -> Option<Locale> {
  Locale::parse(tag).ok().or_else(|| {
    let language = tag.split(['-', '_']).next().unwrap_or_default();
    Locale::parse(language).ok()
  })
}

/// Where [`Negotiator::select`] found the locale it chose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LocaleSource {
  /// The application's preference (a user setting).
  Preference,
  /// The locale cookie.
  Cookie,
  /// The `Accept-Language` header.
  AcceptLanguage,
  /// None of them named a supported locale: the default.
  Default,
}

/// Negotiates request locales against an allowlist of supported locales.
///
/// Matching follows `fluent-langneg`'s lookup: an exact match first, then the supported locale
/// as a range (`de-AT` requested, `de` supported), then likely subtags (`en` requested, `en-US`
/// supported), then a sibling region (`de-CH` requested, `de-AT` supported). The result is
/// always one of [`supported`](Self::supported).
#[derive(Clone, Debug)]
pub struct Negotiator {
  default: Locale,
  supported: Vec<Locale>,
  ids: Vec<LanguageIdentifier>,
}

impl Negotiator {
  /// A negotiator choosing among `supported` (plus `default`, which is always supported) and
  /// falling back to `default`. Order does not matter; duplicates are dropped.
  pub fn new(default: Locale, supported: impl IntoIterator<Item = Locale>) -> Self {
    let mut all = vec![default.clone()];
    for locale in supported {
      if !all.contains(&locale) {
        all.push(locale);
      }
    }
    let ids = all.iter().map(LanguageIdentifier::from).collect();
    Self {
      default,
      supported: all,
      ids,
    }
  }

  /// A negotiator whose allowlist is the locales `catalog` has resources for, with the catalog's
  /// default locale.
  pub fn from_catalog(catalog: &Catalog) -> Self {
    Self::new(catalog.default_locale().clone(), catalog.locales().cloned())
  }

  /// The locale chosen when nothing else matches.
  pub fn default_locale(&self) -> &Locale {
    &self.default
  }

  /// The allowlist, default first.
  pub fn supported(&self) -> &[Locale] {
    &self.supported
  }

  /// Whether `locale` is in the allowlist (exactly).
  pub fn supports(&self, locale: &Locale) -> bool {
    self.supported.contains(locale)
  }

  /// The best supported locale for the requested ones (best first), or `None` when none matches.
  pub fn negotiate<'a>(&self, requested: impl IntoIterator<Item = &'a Locale>) -> Option<Locale> {
    let requested: Vec<LanguageIdentifier> = requested
      .into_iter()
      .map(LanguageIdentifier::from)
      .collect();
    let matched = filter_matches(&requested, &self.ids, NegotiationStrategy::Lookup);
    let first = matched.first()?;
    let index = self.ids.iter().position(|id| std::ptr::eq(*first, id))?;
    Some(self.supported[index].clone())
  }

  /// The best supported locale for an untrusted tag (a cookie or a stored preference), or `None`
  /// when the tag is not a [`Locale`] or matches nothing supported.
  pub fn match_tag(&self, tag: &str) -> Option<Locale> {
    let locale = Locale::parse(tag.trim()).ok()?;
    self.negotiate([&locale])
  }

  /// The best supported locale for an `Accept-Language` value, or `None` when the header is over
  /// the limits ([`parse_accept_language`]) or names nothing supported. The wildcard `*` matches
  /// nothing here: it leaves the choice to the default.
  pub fn match_accept_language(&self, header: &str) -> Option<Locale> {
    let ranges = parse_accept_language(header).ok()?;
    self.negotiate(ranges.iter().filter_map(LanguageRange::locale))
  }

  /// Chooses the locale of a request from an application `preference`, a `cookie` value and an
  /// `Accept-Language` header, in that order; each is used only when it matches a supported
  /// locale. Returns the default with [`LocaleSource::Default`] when none does.
  pub fn select(
    &self,
    preference: Option<&str>,
    cookie: Option<&str>,
    accept_language: Option<&str>,
  ) -> (Locale, LocaleSource) {
    preference
      .and_then(|tag| self.match_tag(tag))
      .map(|locale| (locale, LocaleSource::Preference))
      .or_else(|| {
        cookie
          .and_then(|tag| self.match_tag(tag))
          .map(|locale| (locale, LocaleSource::Cookie))
      })
      .or_else(|| {
        accept_language
          .and_then(|header| self.match_accept_language(header))
          .map(|locale| (locale, LocaleSource::AcceptLanguage))
      })
      .unwrap_or_else(|| (self.default.clone(), LocaleSource::Default))
  }
}

/// A [`Catalog`] and the locale negotiated for one request: everything a view needs to translate.
///
/// The `axum` feature's layer puts one in each request's extensions (and it is an axum
/// extractor); views format with [`format`](Self::format) or, with the `maud` feature,
/// `t(&localizer, "id", args)`.
///
/// ```
/// use id_effect_i18n::{Args, Catalog, Locale, Localizer};
///
/// let en = Locale::parse("en").unwrap();
/// let de = Locale::parse("de").unwrap();
/// let catalog = Catalog::builder(en.clone())
///   .embedded(en, "en.ftl", "hello = Hello, { $name }!")
///   .embedded(de.clone(), "de.ftl", "hello = Hallo, { $name }!")
///   .use_isolating(false)
///   .build()
///   .unwrap();
/// let localizer = Localizer::new(catalog, de);
/// assert_eq!(localizer.format("hello", &Args::new().with("name", "Ada")), "Hallo, Ada!");
/// ```
#[derive(Clone, Debug)]
pub struct Localizer {
  catalog: Catalog,
  locale: Locale,
}

impl Localizer {
  /// Translates from `catalog` in `locale`.
  pub fn new(catalog: Catalog, locale: Locale) -> Self {
    Self { catalog, locale }
  }

  /// The negotiated locale.
  pub fn locale(&self) -> &Locale {
    &self.locale
  }

  /// The catalog messages come from.
  pub fn catalog(&self) -> &Catalog {
    &self.catalog
  }

  /// Formats `key` with `args` in this locale ([`Catalog::format`]: fallback chain, the key
  /// itself when no locale defines it).
  pub fn format(&self, key: &str, args: &Args) -> String {
    self.catalog.format(&self.locale, key, args)
  }

  /// Runs `eff` with the [`CurrentLocale`] in its environment switched to this locale
  /// ([`with_locale`]), so `translate` and `current_locale` inside it see the negotiated locale.
  pub fn scope<A, E, R, J>(&self, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Selector<CurrentLocale, J> + Send + Sync + 'static,
  {
    with_locale(self.locale.clone(), eff)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn loc(tag: &str) -> Locale {
    Locale::parse(tag).unwrap()
  }

  #[test]
  fn quality_values_follow_rfc_9110() {
    assert_eq!(parse_quality("1"), Some(1000));
    assert_eq!(parse_quality("1."), Some(1000));
    assert_eq!(parse_quality("1.000"), Some(1000));
    assert_eq!(parse_quality("0"), Some(0));
    assert_eq!(parse_quality("0.8"), Some(800));
    assert_eq!(parse_quality("0.125"), Some(125));
    assert_eq!(parse_quality("1.5"), None);
    assert_eq!(parse_quality("0.1234"), None);
    assert_eq!(parse_quality("0.x"), None);
    assert_eq!(parse_quality("2"), None);
    assert_eq!(parse_quality(""), None);
  }

  #[test]
  fn ranges_drop_invalid_items_and_reduce_to_language() {
    let ranges =
      parse_accept_language(" en-US-x-twain ;Q=0.9, x-klingon, de;q=oops, fr;q=0, ,*;level=1")
        .unwrap();
    let tags: Vec<_> = ranges
      .iter()
      .map(|r| (r.locale().map(Locale::as_str), r.quality(), r.is_wildcard()))
      .collect();
    assert_eq!(tags, [(None, 1000, true), (Some("en"), 900, false)]);
  }

  #[test]
  fn errors_display() {
    for err in [
      AcceptLanguageError::TooLong,
      AcceptLanguageError::TooManyRanges,
    ] {
      assert!(!err.to_string().is_empty());
    }
  }

  #[test]
  fn negotiator_dedupes_and_reports_allowlist() {
    let n = Negotiator::new(loc("en"), [loc("de"), loc("en"), loc("de")]);
    assert_eq!(n.supported(), [loc("en"), loc("de")]);
    assert_eq!(n.default_locale(), &loc("en"));
    assert!(n.supports(&loc("de")));
    assert!(!n.supports(&loc("de-AT")));
    assert_eq!(n.negotiate([]), None);
    assert_eq!(n.match_tag(" de-AT "), Some(loc("de")));
    assert_eq!(n.match_tag("fr"), None);
    assert_eq!(n.match_accept_language("*"), None);
  }

  #[test]
  fn localizer_scope_switches_current_locale() {
    use id_effect::{Exit, HCons, HNil, hlist, run_test};

    let catalog = Catalog::builder(loc("en"))
      .embedded(loc("en"), "en.ftl", "k = en")
      .embedded(loc("de"), "de.ftl", "k = de")
      .build()
      .unwrap();
    let localizer = Localizer::new(catalog, loc("de"));
    assert_eq!(localizer.locale(), &loc("de"));
    assert_eq!(localizer.catalog().default_locale(), &loc("en"));
    assert_eq!(localizer.format("k", &Args::new()), "de");
    let eff: Effect<Locale, (), HCons<CurrentLocale, HNil>> =
      localizer.scope(crate::current_locale());
    let exit = run_test(eff.provide_all(hlist![CurrentLocale::new(loc("en"))]));
    assert!(matches!(exit, Exit::Success(l) if l == loc("de")));
  }
}
