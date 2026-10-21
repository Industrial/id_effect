//! Internationalization for id_effect services, over [Project Fluent](https://projectfluent.org)
//! (ADR 0010 Amendment 9, release spec 0.13.0 decision D5).
//!
//! - [`Locale`]: a validated, case-normalized language identifier (`de-AT`, `sr-Latn-RS`).
//! - [`Catalog`]: Fluent resources per locale, embedded with `include_str!` or loaded from
//!   allowlisted directories, parsed once at startup; lookups fall back from the requested locale
//!   to its language and then to the default locale; [`Catalog::check`] reports every key a
//!   locale lacks.
//! - CLDR plural categories through Fluent selectors (`{ $n -> [one] … [few] … *[other] … }`),
//!   with the rules of the locale a message comes from.
//! - [`CurrentLocale`]: the locale capability in the environment, switched per request or per
//!   fiber with [`with_locale`]; [`translate`] formats a message in it.
//!
//! ```
//! use id_effect::{Effect, Exit, HCons, HNil, Never, hlist, run_test};
//! use id_effect_i18n::{Args, Catalog, CurrentLocale, Locale, translate, with_locale};
//!
//! let en = Locale::parse("en").unwrap();
//! let pl = Locale::parse("pl").unwrap();
//! let catalog = Catalog::builder(en.clone())
//!   .embedded(en.clone(), "app.ftl", "files = { $n ->\n  [one] one file\n *[other] { $n } files\n}")
//!   .embedded(pl.clone(), "app.ftl", "files = { $n ->
//!     [one] jeden plik
//!     [few] { $n } pliki
//!    *[many] { $n } plików
//!   }")
//!   .use_isolating(false)
//!   .build()
//!   .unwrap();
//! assert!(catalog.check().is_complete());
//!
//! type Env = HCons<Catalog, HCons<CurrentLocale, HNil>>;
//! let eff: Effect<String, Never, Env> = with_locale(pl, translate("files", Args::new().with("n", 3)));
//! let exit = run_test(eff.provide_all(hlist![catalog, CurrentLocale::new(en)]));
//! assert!(matches!(exit, Exit::Success(text) if text == "3 pliki"));
//! ```
//!
//! ## Secure by default (ADR 0013 R4)
//!
//! A locale usually comes from untrusted input (an `Accept-Language` header, a query parameter, a
//! user setting). [`Locale::parse`] reads at most [`Locale::MAX_LEN`] bytes and accepts only ASCII
//! subtags of the documented shapes, and catalogs are keyed by parsed locales: no locale string
//! from input ever reaches a file path. Resource files are read once, at startup, only below
//! directories the application allowlists ([`CatalogBuilder::allow_dir`]; `..` and symbolic
//! links are resolved first), only `.ftl` files of at most [`MAX_RESOURCE_BYTES`]; a parse error
//! fails [`CatalogBuilder::build`]. Message arguments are wrapped in Unicode bidi isolation marks
//! by default.
//!
//! ```
//! use id_effect_i18n::Locale;
//!
//! let locale = Locale::parse("de_at").unwrap();
//! assert_eq!(locale.as_str(), "de-AT");
//! assert_eq!(locale.language(), "de");
//! assert_eq!(locale.region(), Some("AT"));
//! assert!(Locale::parse("../etc/passwd").is_err());
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod args;
mod catalog;
mod current;
mod locale;

pub use args::{ArgValue, Args};
pub use catalog::{
  Catalog, CatalogBuilder, CatalogError, CatalogReport, MAX_RESOURCE_BYTES, MissingKey, Translation,
};
pub use current::{CurrentLocale, current_locale, translate, with_locale};
pub use locale::{Locale, LocaleError};
/// The Unicode language identifier of `unic-langid` (what Fluent and `fluent-langneg` use);
/// convert with `LanguageIdentifier::from(&locale)` and `Locale::try_from(&id)`.
pub use unic_langid::LanguageIdentifier;

/// Compiles and runs the README quick start as a doctest.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
