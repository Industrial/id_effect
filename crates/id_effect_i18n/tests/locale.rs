//! Public-API test of `Locale` (r13-t0-skeletons).

use id_effect_i18n::{Locale, LocaleError};

#[test]
fn locale_round_trips_through_display_and_from_str() {
  for input in ["en", "de-AT", "sr-Latn-RS", "es-419", "zh-Hant"] {
    let locale: Locale = input.parse().unwrap();
    assert_eq!(locale.to_string(), input);
    assert_eq!(Locale::parse(locale.as_str()).unwrap(), locale);
  }
  assert_eq!("".parse::<Locale>(), Err(LocaleError::Empty));
}
