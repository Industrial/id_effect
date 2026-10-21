//! Translated messages in [maud](https://maud.lambda.xyz) views (feature `maud`; web.ui #8).
//!
//! [`t`] formats a message for a request's [`Localizer`] and returns a [`Message`], which
//! implements [`maud::Render`]: a view writes `(t(&localizer, "id", args))` like any other
//! interpolation.
//!
//! ```
//! use id_effect_i18n::{Args, Catalog, Locale, Localizer, t};
//! use maud::html;
//!
//! let en = Locale::parse("en").unwrap();
//! let catalog = Catalog::builder(en.clone())
//!   .embedded(en.clone(), "en.ftl", "greeting = Hello, { $name }!")
//!   .build()
//!   .unwrap();
//! let l = Localizer::new(catalog, en);
//!
//! let page = html! { p { (t(&l, "greeting", Args::new().with("name", "<b>Ada</b>"))) } };
//! assert_eq!(
//!   page.into_string(),
//!   "<p>Hello, \u{2068}&lt;b&gt;Ada&lt;/b&gt;\u{2069}!</p>"
//! );
//! ```
//!
//! ## Secure by default (ADR 0013 R9)
//!
//! A [`Message`] renders as **text**: the whole formatted message, arguments included, goes
//! through maud's HTML escaping, so neither a user-supplied argument nor a translation can inject
//! markup. Arguments keep the catalog's Unicode bidi isolation (U+2068 … U+2069, on unless the
//! catalog was built with `use_isolating(false)`), so a right-to-left name cannot reorder the
//! sentence around it. Markup around a message belongs in the template, not in the catalog.

use std::fmt;

use ::maud::Render;

use crate::{Args, Localizer, Translation};

/// A formatted message, rendered by maud as escaped text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
  text: String,
}

impl Message {
  /// A message with the given (already formatted) text.
  pub fn new(text: impl Into<String>) -> Self {
    Self { text: text.into() }
  }

  /// The formatted text (not escaped).
  pub fn as_str(&self) -> &str {
    &self.text
  }

  /// The formatted text (not escaped).
  pub fn into_string(self) -> String {
    self.text
  }
}

impl fmt::Display for Message {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.text)
  }
}

/// Escaped text: `<`, `>`, `&` and `"` become entities.
impl Render for Message {
  fn render_to(&self, buffer: &mut String) {
    self.text.as_str().render_to(buffer);
  }
}

/// Escaped text, like [`Message`].
impl Render for Translation {
  fn render_to(&self, buffer: &mut String) {
    self.text.as_str().render_to(buffer);
  }
}

/// Formats `key` with `args` in the localizer's locale ([`Localizer::format`]) for a maud view.
pub fn t(localizer: &Localizer, key: &str, args: Args) -> Message {
  Message::new(localizer.format(key, &args))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Catalog, Locale};

  #[test]
  fn message_accessors_and_translation_render() {
    let message = Message::new("a < b");
    assert_eq!(message.as_str(), "a < b");
    assert_eq!(message.to_string(), "a < b");
    assert_eq!(message.render().into_string(), "a &lt; b");
    assert_eq!(message.into_string(), "a < b");

    let en = Locale::parse("en").unwrap();
    let catalog = Catalog::builder(en.clone())
      .embedded(en.clone(), "en.ftl", "k = x & y")
      .build()
      .unwrap();
    let translation = catalog.lookup(&en, "k", &Args::new()).unwrap();
    assert_eq!(translation.render().into_string(), "x &amp; y");
  }
}
