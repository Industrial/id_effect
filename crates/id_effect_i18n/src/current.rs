//! [`CurrentLocale`]: the locale capability, switched per request or fiber with [`with_locale`].

use id_effect::{Effect, FiberRef, Selector, need};

use crate::{Args, Catalog, Locale};

/// Capability: the locale effects format messages in.
///
/// Provide one `CurrentLocale` in the application environment, built with the default locale.
/// Its value is fiber-local (an id_effect [`FiberRef`]): [`with_locale`] switches it for one
/// effect, such as a request handler, and every fiber that effect forks inherits the switch, while
/// other fibers (other requests) keep their own. Nothing is stored in thread-locals, so the value
/// follows the fiber across executor threads.
///
/// ```
/// use id_effect::{Effect, HCons, HNil, Never, Exit, hlist, run_test};
/// use id_effect_i18n::{CurrentLocale, Locale, current_locale, with_locale};
///
/// type Env = HCons<CurrentLocale, HNil>;
///
/// let en = Locale::parse("en").unwrap();
/// let de = Locale::parse("de").unwrap();
/// let both: Effect<(Locale, Locale), Never, Env> = with_locale(de.clone(), current_locale())
///   .flat_map(|inside| current_locale().map(move |after| (inside, after)));
/// let exit = run_test(both.provide_all(hlist![CurrentLocale::new(en.clone())]));
/// assert!(matches!(exit, Exit::Success((inside, after)) if inside == de && after == en));
/// ```
#[derive(Clone, Debug)]
pub struct CurrentLocale {
  cell: FiberRef<Locale>,
}

impl CurrentLocale {
  /// A locale capability whose value is `default` in every fiber until switched.
  pub fn new(default: Locale) -> Self {
    Self {
      cell: FiberRef::new(default),
    }
  }

  /// The value fibers start with.
  pub fn default_locale(&self) -> &Locale {
    self.cell.initial()
  }

  /// The current fiber's locale.
  pub fn get<E, R>(&self) -> Effect<Locale, E, R>
  where
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.cell.get()
  }

  /// Runs `eff` with the locale set to `locale`, then restores the previous value however `eff`
  /// ends. Fibers `eff` forks inherit `locale`.
  pub fn locally<A, E, R>(&self, locale: Locale, eff: Effect<A, E, R>) -> Effect<A, E, R>
  where
    A: Send + 'static,
    E: Send + 'static,
    R: Send + Sync + 'static,
  {
    self.cell.locally(locale, eff)
  }
}

/// The current fiber's locale, read from the [`CurrentLocale`] in the environment.
pub fn current_locale<E, R, J>() -> Effect<Locale, E, R>
where
  E: Send + 'static,
  R: Selector<CurrentLocale, J> + Send + Sync + 'static,
{
  need::<CurrentLocale>().flat_map(|current| current.get())
}

/// Runs `eff` with the [`CurrentLocale`] switched to `locale` (per request or per fiber); the
/// previous locale is restored when `eff` ends, and fibers `eff` forks inherit `locale`.
///
/// `locale` should come from the catalog's supported locales (or a negotiation against them), not
/// straight from a header; an unsupported locale is harmless (lookups fall back to the default)
/// but is what `current_locale` reports.
pub fn with_locale<A, E, R, J>(locale: Locale, eff: Effect<A, E, R>) -> Effect<A, E, R>
where
  A: Send + 'static,
  E: Send + 'static,
  R: Selector<CurrentLocale, J> + Send + Sync + 'static,
{
  need::<CurrentLocale>().flat_map(move |current| current.locally(locale, eff))
}

/// Formats `key` with `args` in the current fiber's locale, from the [`Catalog`] in the
/// environment ([`Catalog::format`]: fallback chain, and the key itself when no locale has it).
///
/// ```
/// use id_effect::{Effect, Exit, HCons, HNil, Never, hlist, run_test};
/// use id_effect_i18n::{Args, Catalog, CurrentLocale, Locale, translate, with_locale};
///
/// let en = Locale::parse("en").unwrap();
/// let de = Locale::parse("de").unwrap();
/// let catalog = Catalog::builder(en.clone())
///   .embedded(en.clone(), "app.ftl", "items = { $n ->\n  [one] one item\n *[other] { $n } items\n}")
///   .embedded(de.clone(), "app.ftl", "items = { $n ->\n  [one] ein Artikel\n *[other] { $n } Artikel\n}")
///   .use_isolating(false)
///   .build()
///   .unwrap();
///
/// let eff: Effect<String, Never, HCons<Catalog, HCons<CurrentLocale, HNil>>> =
///   with_locale(de, translate("items", Args::new().with("n", 3)));
/// let exit = run_test(eff.provide_all(hlist![catalog, CurrentLocale::new(en)]));
/// assert!(matches!(exit, Exit::Success(text) if text == "3 Artikel"));
/// ```
pub fn translate<E, R, J, K>(key: impl Into<String>, args: Args) -> Effect<String, E, R>
where
  E: Send + 'static,
  R: Selector<Catalog, J> + Selector<CurrentLocale, K> + Send + Sync + 'static,
{
  let key = key.into();
  need::<Catalog>().flat_map(move |catalog| {
    current_locale::<E, R, K>().map(move |locale| catalog.format(&locale, &key, &args))
  })
}
