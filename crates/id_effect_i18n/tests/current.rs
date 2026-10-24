//! `CurrentLocale` acceptance tests (r13-e9-4-i18n-core): eco.i18n #7, the locale capability in
//! the environment and `with_locale` switching per request or fiber.

use id_effect::{Effect, Exit, HCons, HNil, fail, hlist, run_test, yield_fiber, zip_par};
use id_effect_i18n::{
  Args, Catalog, CurrentLocale, Locale, current_locale, translate, with_locale,
};

type Env = HCons<Catalog, HCons<CurrentLocale, HNil>>;
type Eff<A> = Effect<A, &'static str, Env>;

fn loc(tag: &str) -> Locale {
  Locale::parse(tag).expect("test locale")
}

fn env() -> Env {
  let catalog = Catalog::builder(loc("en"))
    .embedded(loc("en"), "en.ftl", "greeting = Hello, { $name }!")
    .embedded(loc("de"), "de.ftl", "greeting = Hallo, { $name }!")
    .embedded(loc("pl"), "pl.ftl", "greeting = Cześć, { $name }!")
    .use_isolating(false)
    .build()
    .expect("catalog");
  hlist![catalog, CurrentLocale::new(loc("en"))]
}

fn run<A: Send + 'static>(eff: Eff<A>) -> Exit<A, &'static str> {
  run_test(eff.provide_all(env()))
}

/// One "request": greet, yield to other fibers, then report the locale it still sees.
fn request(tag: &'static str) -> Eff<(String, Locale)> {
  with_locale(
    loc(tag),
    yield_fiber()
      .then(translate("greeting", Args::new().with("name", "Ada")))
      .flat_map(|text| {
        yield_fiber()
          .then(current_locale())
          .map(move |locale| (text, locale))
      }),
  )
}

#[test]
fn with_locale_switches_per_fiber() {
  // Two concurrent requests interleave on the executor; each keeps its own locale.
  let both: Eff<_> = zip_par(request("de"), request("pl"))
    .flat_map(|pair| current_locale().map(move |after| (pair, after)));
  match run(both) {
    Exit::Success((((de_text, de), (pl_text, pl)), after)) => {
      assert_eq!((de_text.as_str(), de), ("Hallo, Ada!", loc("de")));
      assert_eq!((pl_text.as_str(), pl), ("Cześć, Ada!", loc("pl")));
      assert_eq!(after, loc("en"), "the parent fiber keeps the default");
    }
    other => panic!("{other:?}"),
  }

  // A fiber forked inside `with_locale` inherits the locale; its own switch stays its own.
  let nested: Eff<_> = with_locale(
    loc("de"),
    current_locale()
      .fork()
      .widen()
      .flat_map(|inherited| inherited.join())
      .flat_map(|inherited| {
        with_locale(loc("pl"), current_locale())
          .fork()
          .widen()
          .flat_map(|h| h.join())
          .flat_map(move |child| current_locale().map(move |own| (inherited, child, own)))
      }),
  );
  match run(nested) {
    Exit::Success((inherited, child, own)) => {
      assert_eq!(inherited, loc("de"));
      assert_eq!(child, loc("pl"));
      assert_eq!(own, loc("de"));
    }
    other => panic!("{other:?}"),
  }

  // The previous locale is restored however the effect ends.
  let failed: Eff<_> = with_locale(loc("de"), fail::<(), _, _>("boom"))
    .exit()
    .widen()
    .flat_map(|exit| current_locale().map(move |after| (exit, after)));
  match run(failed) {
    Exit::Success((Exit::Failure(_), after)) => assert_eq!(after, loc("en")),
    other => panic!("{other:?}"),
  }
}

#[test]
fn current_locale_defaults_and_translates_in_the_default() {
  let eff: Eff<_> = current_locale().zip(translate("greeting", Args::new().with("name", "Bo")));
  match run(eff) {
    Exit::Success((locale, text)) => {
      assert_eq!(locale, loc("en"));
      assert_eq!(text, "Hello, Bo!");
    }
    other => panic!("{other:?}"),
  }

  // The capability's own methods, without the environment.
  let current = CurrentLocale::new(loc("de"));
  assert_eq!(current.default_locale(), &loc("de"));
  let pair: Effect<(Locale, Locale), &'static str> =
    current.locally(loc("pl"), current.get()).zip(current.get());
  match run_test(pair) {
    Exit::Success((inside, after)) => {
      assert_eq!(inside, loc("pl"));
      assert_eq!(after, loc("de"));
    }
    other => panic!("{other:?}"),
  }
  assert!(format!("{current:?}").contains("CurrentLocale"));
}
