//! `Accept-Language` negotiation acceptance tests (r13-e9-4-i18n-web): eco.i18n #3 and the
//! ADR 0013 R4 limits of the header parse.

use id_effect_i18n::negotiate::{
  AcceptLanguageError, LocaleSource, MAX_ACCEPT_LANGUAGE_BYTES, MAX_ACCEPT_LANGUAGE_RANGES,
  Negotiator, parse_accept_language,
};
use id_effect_i18n::{Catalog, Locale};

fn loc(tag: &str) -> Locale {
  Locale::parse(tag).expect("test locale")
}

/// en (default), de, de-AT, pt-BR.
fn negotiator() -> Negotiator {
  Negotiator::new(loc("en"), [loc("de"), loc("de-AT"), loc("pt-BR")])
}

#[test]
fn negotiates_best_supported_locale() {
  let n = negotiator();
  let best = |header: &str| n.match_accept_language(header);
  // The highest-weighted supported range wins, whatever the order in the header.
  assert_eq!(best("fr;q=0.9, de;q=0.8, en;q=0.1"), Some(loc("de")));
  assert_eq!(best("en;q=0.2, de;q=0.7"), Some(loc("de")));
  // An exact regional match beats the language.
  assert_eq!(best("de-AT, de;q=0.9"), Some(loc("de-AT")));
  // A region we do not have falls back to the language...
  assert_eq!(best("de-CH"), Some(loc("de")));
  // ...and a bare language reaches the regional variant we do have.
  assert_eq!(best("pt"), Some(loc("pt-BR")));
  assert_eq!(best("pt-PT"), Some(loc("pt-BR")));
  // Case and separators are normalized.
  assert_eq!(best("DE_at"), Some(loc("de-AT")));
  // Nothing supported: no match (the caller falls back to the default).
  assert_eq!(best("fr, ja;q=0.5"), None);
  assert_eq!(
    n.select(None, None, Some("fr, de;q=0.5")),
    (loc("de"), LocaleSource::AcceptLanguage)
  );
  assert_eq!(
    n.select(None, None, None),
    (loc("en"), LocaleSource::Default)
  );
}

#[test]
fn preference_beats_cookie_beats_header() {
  let n = negotiator();
  assert_eq!(
    n.select(Some("pt-BR"), Some("de"), Some("en")),
    (loc("pt-BR"), LocaleSource::Preference)
  );
  assert_eq!(
    n.select(None, Some("de"), Some("en")),
    (loc("de"), LocaleSource::Cookie)
  );
  assert_eq!(
    n.select(Some("xx"), Some("de"), Some("en")),
    (loc("de"), LocaleSource::Cookie)
  );
}

#[test]
fn locale_outside_allowlist_falls_back() {
  let n = negotiator();
  // A cookie or preference naming an unsupported (or malformed) locale is ignored...
  assert_eq!(
    n.select(Some("fr"), Some("ja"), Some("de")),
    (loc("de"), LocaleSource::AcceptLanguage)
  );
  assert_eq!(
    n.select(Some("../../etc/passwd"), Some("<script>"), None),
    (loc("en"), LocaleSource::Default)
  );
  // ...and so is a header naming only unsupported locales.
  assert_eq!(
    n.select(None, None, Some("fr-FR, ja, *")),
    (loc("en"), LocaleSource::Default)
  );
  // Whatever the input, the result is in the allowlist.
  for input in ["zz", "x-klingon", "*", "", "en-GB", "de-DE-1901"] {
    let (locale, _) = n.select(Some(input), Some(input), Some(input));
    assert!(n.supports(&locale), "{input} → {locale}");
  }
  // From a catalog: the allowlist is exactly the catalog's locales.
  let catalog = Catalog::builder(loc("en"))
    .embedded(loc("en"), "en.ftl", "k = en")
    .embedded(loc("de"), "de.ftl", "k = de")
    .build()
    .expect("catalog");
  let from_catalog = Negotiator::from_catalog(&catalog);
  assert_eq!(from_catalog.supported(), [loc("en"), loc("de")]);
  assert_eq!(from_catalog.match_accept_language("pt-BR"), None);
}

#[test]
fn r4_accept_language_over_limit_ignored() {
  let n = negotiator();
  // At the limits the header is read.
  let at_ranges = std::iter::repeat_n("fr", MAX_ACCEPT_LANGUAGE_RANGES - 1)
    .chain(["de"])
    .collect::<Vec<_>>()
    .join(",");
  assert_eq!(n.match_accept_language(&at_ranges), Some(loc("de")));
  let at_bytes = format!("de{}", " ".repeat(MAX_ACCEPT_LANGUAGE_BYTES - 2));
  assert_eq!(at_bytes.len(), MAX_ACCEPT_LANGUAGE_BYTES);
  assert_eq!(n.match_accept_language(&at_bytes), Some(loc("de")));

  // One range or one byte more and the whole header is ignored (default locale).
  let over_ranges = format!("{at_ranges},de");
  assert_eq!(
    parse_accept_language(&over_ranges),
    Err(AcceptLanguageError::TooManyRanges)
  );
  assert_eq!(
    n.select(None, None, Some(&over_ranges)),
    (loc("en"), LocaleSource::Default)
  );
  let over_bytes = format!("{at_bytes} ");
  assert_eq!(
    parse_accept_language(&over_bytes),
    Err(AcceptLanguageError::TooLong)
  );
  assert_eq!(
    n.select(None, None, Some(&over_bytes)),
    (loc("en"), LocaleSource::Default)
  );
  // A huge header fails fast on its length, before any splitting.
  let huge = "de,".repeat(100_000);
  assert_eq!(
    parse_accept_language(&huge),
    Err(AcceptLanguageError::TooLong)
  );
  // An over-long tag is not a locale and is dropped (Locale::MAX_LEN).
  assert!(
    parse_accept_language(&"a".repeat(40))
      .expect("in limits")
      .is_empty()
  );
}

#[test]
fn parse_orders_by_weight_and_drops_unacceptable() {
  let ranges = parse_accept_language("da, en-GB;q=0.8, en;q=0.7, fr;q=0, *;q=0.1").unwrap();
  let tags: Vec<_> = ranges
    .iter()
    .map(|r| (r.locale().map(Locale::as_str), r.quality()))
    .collect();
  assert_eq!(
    tags,
    [
      (Some("da"), 1000),
      (Some("en-GB"), 800),
      (Some("en"), 700),
      (None, 100)
    ]
  );
  assert!(parse_accept_language("").unwrap().is_empty());
  assert!(parse_accept_language(" , ,").unwrap().is_empty());
  assert!(parse_accept_language("de;q=1.5").unwrap().is_empty());
}
