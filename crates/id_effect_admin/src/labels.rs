//! The words on admin pages: entity and field labels and the page chrome, in English unless the
//! site has an `id_effect_i18n::Catalog` (feature `i18n`).
//!
//! With a catalog, the label keys are Fluent messages looked up in the request's locale (falling
//! back to its language and the catalog's default locale):
//!
//! - an entity's [`Entity::LABEL`](id_effect_sql::Entity::LABEL) (`document`) is a message;
//! - a field's [`FieldMeta::label`](id_effect_sql::FieldMeta) (`document.title`) is the attribute
//!   `title` of the message `document`;
//! - the page chrome is the message `admin` and its attributes (see [`UI_MESSAGES`]).
//!
//! A key no locale defines shows the English default (the humanized field name, for example
//! `Created at`), so a partial catalog never leaves a page blank. The request's locale is the
//! `CurrentLocale` an `id_effect_i18n` axum layer records in the request capabilities, else a
//! `Locale` request extension, else the catalog's default.

use axum::http::request::Parts;

/// The page chrome's message keys and their English text (`{ $count }` is the argument of
/// `admin.results`). A catalog translating the admin defines the message `admin` with these
/// attributes.
pub const UI_MESSAGES: &[(&str, &str)] = &[
  ("admin", "Administration"),
  ("admin.search", "Search"),
  ("admin.apply", "Apply"),
  ("admin.any", "Any"),
  ("admin.yes", "Yes"),
  ("admin.no", "No"),
  ("admin.results", "{ $count } results"),
  ("admin.empty", "No records match."),
  ("admin.previous", "Previous"),
  ("admin.next", "Next"),
  ("admin.back", "Back to the list"),
  ("admin.nothing", "Nothing to administer."),
  ("admin.forbidden", "You may not do this."),
  ("admin.not-found", "This record does not exist."),
  ("admin.bad-request", "This request is not valid."),
  ("admin.error", "Something went wrong."),
  ("admin.new", "New"),
  ("admin.create", "Create"),
  ("admin.edit", "Edit"),
  ("admin.save", "Save"),
  ("admin.cancel", "Cancel"),
  ("admin.delete", "Delete"),
  (
    "admin.delete-confirm",
    "Delete this record? This cannot be undone.",
  ),
  ("admin.invalid", "Please correct the errors below."),
  (
    "admin.conflict",
    "Someone else changed this record. Reload it and try again.",
  ),
  ("admin.duplicate", "A record with this key already exists."),
  (
    "admin.csrf",
    "This form has expired. Reload the page and try again.",
  ),
  ("admin.too-large", "This form is too large."),
  ("admin.too-many", "Too many records are selected."),
  ("admin.select", "Select"),
  ("admin.selected", "With the selected records:"),
  ("admin.run", "Run"),
  ("admin.find", "Find"),
  ("admin.add", "Add"),
  ("admin.remove", "Remove"),
  ("admin.none", "None."),
];

/// The English text of the chrome message `key`.
fn english(key: &str) -> &'static str {
  UI_MESSAGES
    .iter()
    .find(|(k, _)| *k == key)
    .map_or("", |(_, text)| text)
}

/// The labels of one request.
#[derive(Clone, Debug, Default)]
pub(crate) struct Labels {
  #[cfg(feature = "i18n")]
  i18n: Option<(id_effect_i18n::Catalog, id_effect_i18n::Locale)>,
}

impl Labels {
  /// The labels for the request `parts` with the site's catalog, if any.
  #[cfg_attr(not(feature = "i18n"), allow(unused_variables))]
  pub(crate) fn for_request(site: &crate::site::SiteShared, parts: &Parts) -> Self {
    #[cfg(feature = "i18n")]
    {
      let i18n = site.catalog.as_ref().map(|catalog| {
        let locale = parts
          .extensions
          .get::<id_effect_axum::RequestCaps>()
          .and_then(|caps| caps.get::<id_effect_i18n::CurrentLocale>())
          .map(|current| current.default_locale().clone())
          .or_else(|| parts.extensions.get::<id_effect_i18n::Locale>().cloned())
          .unwrap_or_else(|| catalog.default_locale().clone());
        (catalog.clone(), locale)
      });
      Self { i18n }
    }
    #[cfg(not(feature = "i18n"))]
    Self::default()
  }

  /// The translation of `key` with `count`, if the catalog has one.
  #[cfg_attr(not(feature = "i18n"), allow(unused_variables))]
  fn lookup(&self, key: &str, count: Option<u64>) -> Option<String> {
    #[cfg(feature = "i18n")]
    if let Some((catalog, locale)) = &self.i18n {
      let mut args = id_effect_i18n::Args::new();
      if let Some(count) = count {
        args.set("count", count);
      }
      return catalog.lookup(locale, key, &args).map(|t| t.text);
    }
    None
  }

  /// The label `key`, else `fallback`.
  pub(crate) fn label(&self, key: &str, fallback: impl FnOnce() -> String) -> String {
    self.lookup(key, None).unwrap_or_else(fallback)
  }

  /// The chrome message `key` (one of [`UI_MESSAGES`]).
  pub(crate) fn ui(&self, key: &str) -> String {
    self.label(key, || english(key).to_owned())
  }

  /// The validation `message` (a `Schema` issue's English message) in the request's language:
  /// the `id_effect_i18n` schema message of its code, or the message id it is itself; `None`
  /// without a catalog or a translation.
  #[cfg_attr(not(feature = "i18n"), allow(unused_variables))]
  pub(crate) fn schema_message(&self, message: &str) -> Option<String> {
    #[cfg(feature = "i18n")]
    if let Some((catalog, locale)) = &self.i18n {
      let issue = id_effect::schema::ParseError::new("", message);
      let translated = id_effect_i18n::translate_parse_error(catalog, locale, &issue).message;
      return (translated != message).then_some(translated);
    }
    None
  }

  /// `admin.results` for `count` rows.
  pub(crate) fn results(&self, count: u64) -> String {
    self
      .lookup("admin.results", Some(count))
      .unwrap_or_else(|| english("admin.results").replace("{ $count }", &count.to_string()))
  }

  /// The language of the page (`<html lang>`).
  pub(crate) fn lang(&self) -> String {
    #[cfg(feature = "i18n")]
    if let Some((_, locale)) = &self.i18n {
      return locale.as_str().to_owned();
    }
    "en".to_owned()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn english_chrome_without_a_catalog() {
    let labels = Labels::default();
    assert_eq!(labels.ui("admin.next"), "Next");
    assert_eq!(labels.ui("admin.unknown"), "");
    assert_eq!(labels.results(3), "3 results");
    assert_eq!(labels.label("document.title", || "Title".into()), "Title");
    assert_eq!(labels.lang(), "en");
    assert_eq!(labels.schema_message("expected i64"), None);
    assert!(
      UI_MESSAGES
        .iter()
        .all(|(k, v)| k.starts_with("admin") && !v.is_empty())
    );
  }
}
