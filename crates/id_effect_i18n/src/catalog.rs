//! [`Catalog`]: Project Fluent resources per locale, loaded once at startup.
//!
//! A catalog is built by a [`CatalogBuilder`] from resources embedded in the binary
//! ([`CatalogBuilder::embedded`], usually with `include_str!`) or read from directories the
//! application allowlists first ([`CatalogBuilder::allow_dir`]). Every resource is parsed while the
//! catalog is built: a syntax error, a duplicate message or a missing default locale fails
//! [`build`](CatalogBuilder::build), so a broken translation stops the service at startup instead
//! of surfacing in a response.
//!
//! Lookups walk a fallback chain: the requested locale, its language (`de` for `de-AT`), then the
//! default locale. [`Catalog::check`] reports every key of the default locale that a non-default
//! locale lacks (and would therefore show in the default language).

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fluent_bundle::FluentResource;
use fluent_bundle::concurrent::FluentBundle;
use fluent_syntax::ast;
use unic_langid::LanguageIdentifier;

use crate::{Args, Locale};

/// The largest Fluent resource file a catalog reads, in bytes (1 MiB). A larger file fails the
/// build with [`CatalogError::TooLarge`].
pub const MAX_RESOURCE_BYTES: u64 = 1 << 20;

type Bundle = FluentBundle<FluentResource>;

/// One locale's messages: its Fluent bundle and the keys (`id`, `id.attribute`) it defines.
struct LocaleCatalog {
  bundle: Bundle,
  keys: BTreeSet<String>,
}

impl LocaleCatalog {
  fn new(locale: &Locale, use_isolating: bool) -> Self {
    let mut bundle = Bundle::new_concurrent(vec![LanguageIdentifier::from(locale)]);
    bundle.set_use_isolating(use_isolating);
    Self {
      bundle,
      keys: BTreeSet::new(),
    }
  }

  fn add(&mut self, source: Source) -> Result<(), CatalogError> {
    let Source {
      locale,
      origin,
      text,
    } = source;
    let resource = match FluentResource::try_new(text.into_owned()) {
      Ok(resource) => resource,
      Err((_, errors)) => {
        return Err(CatalogError::Parse {
          locale,
          origin,
          errors: errors.iter().map(ToString::to_string).collect(),
        });
      }
    };
    let keys = message_keys(&resource);
    if let Err(errors) = self.bundle.add_resource(resource) {
      return Err(CatalogError::Conflict {
        locale,
        origin,
        errors: errors.iter().map(ToString::to_string).collect(),
      });
    }
    self.keys.extend(keys);
    Ok(())
  }
}

/// The keys a resource defines: `id` for a message with a value, `id.attribute` per attribute.
/// Terms (`-brand`) are private to the resource and are not keys.
fn message_keys(resource: &FluentResource) -> Vec<String> {
  let mut keys = Vec::new();
  for entry in resource.entries() {
    if let ast::Entry::Message(message) = entry {
      let id = message.id.name;
      if message.value.is_some() {
        keys.push(id.to_owned());
      }
      keys.extend(
        message
          .attributes
          .iter()
          .map(|attribute| format!("{id}.{}", attribute.id.name)),
      );
    }
  }
  keys
}

/// One resource waiting to be parsed by [`CatalogBuilder::build`].
#[derive(Debug)]
struct Source {
  locale: Locale,
  origin: String,
  text: Cow<'static, str>,
}

struct Inner {
  default: Locale,
  locales: BTreeMap<Locale, LocaleCatalog>,
}

/// Fluent messages per locale, with a fallback chain and a completeness check.
///
/// Cheap to clone (one `Arc`), `Send + Sync`: provide it once in the application environment and
/// read it with `need::<Catalog>()` or [`translate`](crate::translate).
///
/// ```
/// use id_effect_i18n::{Args, Catalog, Locale};
///
/// let en = Locale::parse("en").unwrap();
/// let de = Locale::parse("de").unwrap();
/// let catalog = Catalog::builder(en.clone())
///   .embedded(en.clone(), "app.ftl", "hello = Hello, { $name }!\nbye = Goodbye")
///   .embedded(de.clone(), "app.ftl", "hello = Hallo, { $name }!")
///   .use_isolating(false)
///   .build()
///   .unwrap();
///
/// let args = Args::new().with("name", "Ada");
/// let de_at = Locale::parse("de-AT").unwrap();
/// assert_eq!(catalog.format(&de_at, "hello", &args), "Hallo, Ada!"); // de-AT → de
/// assert_eq!(catalog.format(&de, "bye", &Args::new()), "Goodbye"); // de → en
/// assert_eq!(catalog.check().missing().len(), 1); // `bye` is missing from de
/// ```
#[derive(Clone)]
pub struct Catalog {
  inner: Arc<Inner>,
}

impl fmt::Debug for Catalog {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Catalog")
      .field("default", &self.inner.default)
      .field("locales", &self.inner.locales.keys().collect::<Vec<_>>())
      .finish()
  }
}

/// A formatted message and the locale whose resource produced it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Translation {
  /// The formatted text.
  pub text: String,
  /// The locale the message came from: the requested one, its language, or the default.
  pub locale: Locale,
}

impl Catalog {
  /// A builder for a catalog whose fallback of last resort is `default`.
  pub fn builder(default: Locale) -> CatalogBuilder {
    CatalogBuilder {
      default,
      roots: Vec::new(),
      sources: Vec::new(),
      use_isolating: true,
      require_complete: false,
    }
  }

  /// The default locale: the last step of every fallback chain.
  pub fn default_locale(&self) -> &Locale {
    &self.inner.default
  }

  /// Every locale with at least one resource, in order.
  pub fn locales(&self) -> impl Iterator<Item = &Locale> {
    self.inner.locales.keys()
  }

  /// Whether `locale` has resources of its own (the allowlist of locales a request may select).
  pub fn supports(&self, locale: &Locale) -> bool {
    self.inner.locales.contains_key(locale)
  }

  /// Whether `locale`'s own resources define `key` (`id` or `id.attribute`), without fallback.
  pub fn has_key(&self, locale: &Locale, key: &str) -> bool {
    self
      .inner
      .locales
      .get(locale)
      .is_some_and(|entry| entry.keys.contains(key))
  }

  /// The locales a lookup for `locale` tries, in order: `locale`, its language, the default; each
  /// once, and only those with resources.
  pub fn fallback_chain(&self, locale: &Locale) -> Vec<&Locale> {
    let language = locale.language_only();
    let mut chain: Vec<&Locale> = Vec::with_capacity(3);
    for candidate in [locale, &language, &self.inner.default] {
      if let Some((known, _)) = self.inner.locales.get_key_value(candidate)
        && !chain.contains(&known)
      {
        chain.push(known);
      }
    }
    chain
  }

  /// Formats `key` (`id` or `id.attribute`) with `args` in the first locale of
  /// [`fallback_chain`](Self::fallback_chain) that defines it; `None` when none does.
  ///
  /// Plural and other selectors use the CLDR rules of the locale the message came from. A
  /// placeable whose argument is missing renders as `{$name}` (Fluent's behavior). Arguments are
  /// wrapped in Unicode bidi isolation marks (U+2068 … U+2069) unless the catalog was built with
  /// [`use_isolating(false)`](CatalogBuilder::use_isolating).
  pub fn lookup(&self, locale: &Locale, key: &str, args: &Args) -> Option<Translation> {
    let (id, attribute) = match key.split_once('.') {
      Some((id, attribute)) => (id, Some(attribute)),
      None => (key, None),
    };
    let fluent_args = args.to_fluent();
    self.fallback_chain(locale).into_iter().find_map(|known| {
      let bundle = &self.inner.locales[known].bundle;
      let message = bundle.get_message(id)?;
      let pattern = match attribute {
        Some(name) => message.get_attribute(name)?.value(),
        None => message.value()?,
      };
      let mut errors = Vec::new();
      let text = bundle
        .format_pattern(pattern, Some(&fluent_args), &mut errors)
        .into_owned();
      Some(Translation {
        text,
        locale: known.clone(),
      })
    })
  }

  /// Formats `key` like [`lookup`](Self::lookup), or returns `key` itself when no locale of the
  /// chain defines it (so a missing message is visible, never empty).
  pub fn format(&self, locale: &Locale, key: &str, args: &Args) -> String {
    self
      .lookup(locale, key, args)
      .map_or_else(|| key.to_owned(), |translation| translation.text)
  }

  /// The startup completeness check: every key of the default locale that a non-default locale
  /// lacks, where neither the locale nor its language defines it (so it would show in the
  /// default language).
  pub fn check(&self) -> CatalogReport {
    let mut missing = Vec::new();
    // `build` refuses a catalog without the default locale, so the index cannot fail.
    let default = &self.inner.locales[&self.inner.default];
    for (locale, entry) in &self.inner.locales {
      if *locale == self.inner.default {
        continue;
      }
      let parent = self.inner.locales.get(&locale.language_only());
      for key in &default.keys {
        let covered = entry.keys.contains(key) || parent.is_some_and(|p| p.keys.contains(key));
        if !covered {
          missing.push(MissingKey {
            locale: locale.clone(),
            key: key.clone(),
          });
        }
      }
    }
    CatalogReport { missing }
  }
}

/// Builds a [`Catalog`]; see [`Catalog::builder`].
///
/// Files are read only below directories allowlisted with [`allow_dir`](Self::allow_dir) (after
/// resolving `..` and symbolic links), only with the `.ftl` extension and only up to
/// [`MAX_RESOURCE_BYTES`]. Locales are never taken from request input here: a locale directory's
/// name is parsed as a [`Locale`] at startup, and lookups go through the parsed catalog, never
/// the file system.
#[derive(Debug)]
pub struct CatalogBuilder {
  default: Locale,
  roots: Vec<PathBuf>,
  sources: Vec<Source>,
  use_isolating: bool,
  require_complete: bool,
}

impl CatalogBuilder {
  /// Adds a resource compiled into the binary, e.g.
  /// `.embedded(de, "app.ftl", include_str!("../locales/de/app.ftl"))`. `origin` names it in
  /// errors. It is parsed by [`build`](Self::build).
  #[must_use]
  pub fn embedded(mut self, locale: Locale, origin: &'static str, source: &'static str) -> Self {
    self.sources.push(Source {
      locale,
      origin: origin.to_owned(),
      text: Cow::Borrowed(source),
    });
    self
  }

  /// Adds a resource from a string the application owns (a trusted source, not request input).
  #[must_use]
  pub fn source(
    mut self,
    locale: Locale,
    origin: impl Into<String>,
    text: impl Into<String>,
  ) -> Self {
    self.sources.push(Source {
      locale,
      origin: origin.into(),
      text: Cow::Owned(text.into()),
    });
    self
  }

  /// Allowlists `dir`: [`file`](Self::file) and [`dir`](Self::dir) read only below an allowlisted
  /// directory. Without one, every path is refused.
  ///
  /// # Errors
  ///
  /// [`CatalogError::Io`] when `dir` cannot be resolved (it does not exist, for example).
  pub fn allow_dir(mut self, dir: impl AsRef<Path>) -> Result<Self, CatalogError> {
    let dir = dir.as_ref();
    let root = fs::canonicalize(dir).map_err(|e| CatalogError::io(dir, &e))?;
    self.roots.push(root);
    Ok(self)
  }

  /// Adds the Fluent file at `path` as a resource of `locale`.
  ///
  /// # Errors
  ///
  /// [`CatalogError::PathOutsideAllowlist`] when the resolved path is not below an allowlisted
  /// directory, [`CatalogError::NotFluentFile`] without the `.ftl` extension,
  /// [`CatalogError::TooLarge`] above [`MAX_RESOURCE_BYTES`], [`CatalogError::Io`] when it
  /// cannot be read as UTF-8.
  pub fn file(mut self, locale: Locale, path: impl AsRef<Path>) -> Result<Self, CatalogError> {
    let path = path.as_ref();
    let resolved = self.resolve(path)?;
    if resolved.extension() != Some(OsStr::new("ftl")) {
      return Err(CatalogError::NotFluentFile {
        path: path.to_path_buf(),
      });
    }
    let mut text = String::new();
    File::open(&resolved)
      .and_then(|file| file.take(MAX_RESOURCE_BYTES + 1).read_to_string(&mut text))
      .map_err(|e| CatalogError::io(path, &e))?;
    if text.len() as u64 > MAX_RESOURCE_BYTES {
      return Err(CatalogError::TooLarge {
        path: path.to_path_buf(),
        limit: MAX_RESOURCE_BYTES,
      });
    }
    self.sources.push(Source {
      locale,
      origin: path.display().to_string(),
      text: Cow::Owned(text),
    });
    Ok(self)
  }

  /// Adds every `<dir>/<locale>/*.ftl` file, in name order: each subdirectory of `dir` is one
  /// locale (`en`, `de`, `pt-BR` or `pt_BR`). Files directly in `dir`, other extensions and
  /// nested directories are ignored.
  ///
  /// # Errors
  ///
  /// [`CatalogError::InvalidLocaleDir`] for a subdirectory whose name is not a locale, and every
  /// error of [`file`](Self::file) for the files.
  pub fn dir(mut self, dir: impl AsRef<Path>) -> Result<Self, CatalogError> {
    let root = self.resolve(dir.as_ref())?;
    for locale_dir in sorted_entries(&root)? {
      if !locale_dir.is_dir() {
        continue;
      }
      let locale = locale_dir
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(|name| Locale::parse(name).ok())
        .ok_or_else(|| CatalogError::InvalidLocaleDir {
          path: locale_dir.clone(),
        })?;
      for file in sorted_entries(&locale_dir)? {
        if file.is_file() && file.extension() == Some(OsStr::new("ftl")) {
          self = self.file(locale.clone(), &file)?;
        }
      }
    }
    Ok(self)
  }

  /// Whether arguments are wrapped in Unicode bidi isolation marks (U+2068 … U+2069) so a
  /// right-to-left name cannot reorder the surrounding text. On by default; turn it off only for
  /// output that is never shown in a bidirectional context (logs, tests).
  #[must_use]
  pub fn use_isolating(mut self, on: bool) -> Self {
    self.use_isolating = on;
    self
  }

  /// Makes [`build`](Self::build) fail with [`CatalogError::Incomplete`] when
  /// [`Catalog::check`] reports a missing key (for CI, or a strict production profile).
  #[must_use]
  pub fn require_complete(mut self) -> Self {
    self.require_complete = true;
    self
  }

  /// Parses every resource and builds the catalog.
  ///
  /// # Errors
  ///
  /// [`CatalogError::Parse`] for a Fluent syntax error, [`CatalogError::Conflict`] for a message
  /// defined twice in one locale, [`CatalogError::MissingDefault`] when the default locale has no
  /// resource, [`CatalogError::Incomplete`] under [`require_complete`](Self::require_complete).
  pub fn build(self) -> Result<Catalog, CatalogError> {
    let Self {
      default,
      sources,
      use_isolating,
      require_complete,
      ..
    } = self;
    let mut locales: BTreeMap<Locale, LocaleCatalog> = BTreeMap::new();
    for source in sources {
      locales
        .entry(source.locale.clone())
        .or_insert_with(|| LocaleCatalog::new(&source.locale, use_isolating))
        .add(source)?;
    }
    if !locales.contains_key(&default) {
      return Err(CatalogError::MissingDefault { locale: default });
    }
    let catalog = Catalog {
      inner: Arc::new(Inner { default, locales }),
    };
    if require_complete {
      let report = catalog.check();
      if !report.is_complete() {
        return Err(CatalogError::Incomplete(report));
      }
    }
    Ok(catalog)
  }

  /// `path` resolved (`..` and symbolic links), if it is below an allowlisted directory.
  fn resolve(&self, path: &Path) -> Result<PathBuf, CatalogError> {
    let resolved = fs::canonicalize(path).map_err(|e| CatalogError::io(path, &e))?;
    if self.roots.iter().any(|root| resolved.starts_with(root)) {
      Ok(resolved)
    } else {
      Err(CatalogError::PathOutsideAllowlist {
        path: path.to_path_buf(),
      })
    }
  }
}

/// The entries of `dir`, sorted by path.
fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>, CatalogError> {
  let mut entries = fs::read_dir(dir)
    .and_then(|entries| {
      entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
    })
    .map_err(|e| CatalogError::io(dir, &e))?;
  entries.sort();
  Ok(entries)
}

/// One key of the default locale that a locale lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingKey {
  /// The locale without the key.
  pub locale: Locale,
  /// The key (`id` or `id.attribute`).
  pub key: String,
}

impl fmt::Display for MissingKey {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{}: {}", self.locale, self.key)
  }
}

/// What [`Catalog::check`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogReport {
  missing: Vec<MissingKey>,
}

impl CatalogReport {
  /// Every missing key, by locale then key.
  pub fn missing(&self) -> &[MissingKey] {
    &self.missing
  }

  /// The keys `locale` lacks.
  pub fn missing_in<'a>(&'a self, locale: &'a Locale) -> impl Iterator<Item = &'a str> {
    self
      .missing
      .iter()
      .filter(move |m| m.locale == *locale)
      .map(|m| m.key.as_str())
  }

  /// Whether every locale has every key of the default locale.
  pub fn is_complete(&self) -> bool {
    self.missing.is_empty()
  }
}

impl fmt::Display for CatalogReport {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    if self.missing.is_empty() {
      return f.write_str("every locale has every key of the default locale");
    }
    write!(f, "{} missing key(s):", self.missing.len())?;
    for missing in &self.missing {
      write!(f, " {missing};")?;
    }
    Ok(())
  }
}

/// Why a [`CatalogBuilder`] refused a path or failed to build.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CatalogError {
  /// A file or directory could not be resolved or read.
  Io {
    /// The path as given.
    path: PathBuf,
    /// The operating system's message.
    message: String,
  },
  /// The resolved path is not below an allowlisted directory.
  PathOutsideAllowlist {
    /// The path as given.
    path: PathBuf,
  },
  /// The file does not have the `.ftl` extension.
  NotFluentFile {
    /// The path as given.
    path: PathBuf,
  },
  /// The file is larger than [`MAX_RESOURCE_BYTES`].
  TooLarge {
    /// The path as given.
    path: PathBuf,
    /// The limit in bytes.
    limit: u64,
  },
  /// A subdirectory of a [`CatalogBuilder::dir`] is not named after a locale.
  InvalidLocaleDir {
    /// The subdirectory.
    path: PathBuf,
  },
  /// A resource is not valid Fluent syntax.
  Parse {
    /// The resource's locale.
    locale: Locale,
    /// The resource (file path or embedded name).
    origin: String,
    /// The parser's messages.
    errors: Vec<String>,
  },
  /// A resource defines a message or term that another resource of the locale already defines.
  Conflict {
    /// The resource's locale.
    locale: Locale,
    /// The resource (file path or embedded name).
    origin: String,
    /// Fluent's messages.
    errors: Vec<String>,
  },
  /// The default locale has no resource.
  MissingDefault {
    /// The default locale.
    locale: Locale,
  },
  /// [`CatalogBuilder::require_complete`] is on and a locale lacks keys.
  Incomplete(CatalogReport),
}

impl CatalogError {
  fn io(path: &Path, error: &std::io::Error) -> Self {
    Self::Io {
      path: path.to_path_buf(),
      message: error.to_string(),
    }
  }
}

impl fmt::Display for CatalogError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Io { path, message } => write!(f, "cannot read {}: {message}", path.display()),
      Self::PathOutsideAllowlist { path } => write!(
        f,
        "{} is outside the allowlisted catalog directories",
        path.display()
      ),
      Self::NotFluentFile { path } => write!(f, "{} is not a .ftl file", path.display()),
      Self::TooLarge { path, limit } => {
        write!(f, "{} is larger than {limit} bytes", path.display())
      }
      Self::InvalidLocaleDir { path } => {
        write!(f, "{} is not named after a locale", path.display())
      }
      Self::Parse {
        locale,
        origin,
        errors,
      } => write!(
        f,
        "Fluent syntax error in {origin} ({locale}): {}",
        errors.join("; ")
      ),
      Self::Conflict {
        locale,
        origin,
        errors,
      } => write!(
        f,
        "{origin} ({locale}) redefines messages: {}",
        errors.join("; ")
      ),
      Self::MissingDefault { locale } => {
        write!(f, "the default locale {locale} has no resource")
      }
      Self::Incomplete(report) => write!(f, "incomplete catalog: {report}"),
    }
  }
}

impl std::error::Error for CatalogError {}
