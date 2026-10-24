//! Parse and normalize locales the way a request handler receives them.
//!
//! ```bash
//! cargo run -p id_effect_i18n --example 010_locale
//! ```

use id_effect_i18n::Locale;

fn main() {
  for input in ["en", "de_at", "sr-latn-rs", "es-419", "../etc", ""] {
    match Locale::parse(input) {
      Ok(locale) => println!(
        "{input:?} -> {locale} (fallback {})",
        locale.language_only()
      ),
      Err(err) => println!("{input:?} rejected: {err}"),
    }
  }
}
