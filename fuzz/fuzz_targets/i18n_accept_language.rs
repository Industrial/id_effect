//! The `Accept-Language` header (id_effect_i18n `negotiate::parse_accept_language`, 0.13), then
//! what the request layer does with it: `Negotiator::select` against a fixed allowlist, with the
//! input also tried as the locale cookie and the stored preference (both untrusted tags).
//!
//! Checks besides "no panic": a value over `MAX_ACCEPT_LANGUAGE_BYTES` is refused as a whole and
//! an accepted one has at most `MAX_ACCEPT_LANGUAGE_RANGES` ranges, best first, each with a weight
//! in `1..=1000` and a locale of at most `Locale::MAX_LEN` bytes that re-parses to itself; the
//! negotiated locale is always one of the allowlist (no header value reaches a catalog lookup).
#![no_main]

use id_effect_i18n::Locale;
use id_effect_i18n::negotiate::{
  AcceptLanguageError, LocaleSource, MAX_ACCEPT_LANGUAGE_BYTES, MAX_ACCEPT_LANGUAGE_RANGES,
  Negotiator, parse_accept_language,
};
use libfuzzer_sys::fuzz_target;

fn negotiator() -> Negotiator {
  let tag = |t: &str| Locale::parse(t).expect("an allowlisted locale");
  Negotiator::new(
    tag("en"),
    [tag("de"), tag("de-AT"), tag("fr-CA"), tag("zh-Hant")],
  )
}

fuzz_target!(|data: &[u8]| {
  let Ok(header) = std::str::from_utf8(data) else {
    return;
  };
  match parse_accept_language(header) {
    Ok(ranges) => {
      assert!(
        header.len() <= MAX_ACCEPT_LANGUAGE_BYTES,
        "a header over the byte cap"
      );
      assert!(
        ranges.len() <= MAX_ACCEPT_LANGUAGE_RANGES,
        "more ranges than the cap"
      );
      for pair in ranges.windows(2) {
        assert!(
          pair[0].quality() >= pair[1].quality(),
          "ranges are not best first"
        );
      }
      for range in &ranges {
        assert!(
          (1..=1000).contains(&range.quality()),
          "weight {}",
          range.quality()
        );
        assert_eq!(range.is_wildcard(), range.locale().is_none());
        if let Some(locale) = range.locale() {
          assert!(
            locale.as_str().len() <= Locale::MAX_LEN,
            "an over-long locale"
          );
          assert_eq!(
            Locale::parse(locale.as_str()).ok().as_ref(),
            Some(locale),
            "a locale has one spelling"
          );
        }
      }
    }
    Err(AcceptLanguageError::TooLong) => assert!(header.len() > MAX_ACCEPT_LANGUAGE_BYTES),
    Err(AcceptLanguageError::TooManyRanges) => {}
  }

  let negotiator = negotiator();
  let (chosen, source) = negotiator.select(None, None, Some(header));
  assert!(negotiator.supports(&chosen), "{chosen} is not allowlisted");
  if source == LocaleSource::Default {
    assert_eq!(&chosen, negotiator.default_locale());
  }
  for (preference, cookie) in [(Some(header), None), (None, Some(header))] {
    let (chosen, _) = negotiator.select(preference, cookie, None);
    assert!(negotiator.supports(&chosen), "{chosen} is not allowlisted");
  }
});
