//! Build a Fluent catalog from embedded resources, check it at startup, and format plurals in
//! the locale a fiber selected with `with_locale`.
//!
//! ```bash
//! cargo run -p id_effect_i18n --example 020_catalog
//! ```

use id_effect::{Effect, Exit, HCons, HNil, Never, hlist, run_test};
use id_effect_i18n::{Args, Catalog, CurrentLocale, Locale, translate, with_locale};

const EN: &str = "\
inbox = { $count ->
    [one] You have one new message.
   *[other] You have { $count } new messages.
}
signed-in = Signed in as { $name }.
";

const DE: &str = "\
inbox = { $count ->
    [one] Sie haben eine neue Nachricht.
   *[other] Sie haben { $count } neue Nachrichten.
}
";

type Env = HCons<Catalog, HCons<CurrentLocale, HNil>>;

fn main() {
  let en = Locale::parse("en").expect("en");
  let catalog = Catalog::builder(en.clone())
    .embedded(en.clone(), "en/app.ftl", EN)
    .embedded(Locale::parse("de").expect("de"), "de/app.ftl", DE)
    .use_isolating(false) // plain terminal output
    .build()
    .expect("every resource parses");

  // The startup check: `signed-in` is missing from de and would show in English.
  println!("check: {}", catalog.check());

  for (tag, count) in [("en", 1), ("en", 5), ("de-AT", 1), ("de", 5)] {
    let locale = Locale::parse(tag).expect("locale");
    let eff: Effect<String, Never, Env> =
      with_locale(locale, translate("inbox", Args::new().with("count", count)));
    let env = hlist![catalog.clone(), CurrentLocale::new(en.clone())];
    match run_test(eff.provide_all(env)) {
      Exit::Success(text) => println!("{tag:>5} {count}: {text}"),
      Exit::Failure(cause) => println!("{tag:>5} {count}: failed: {cause:?}"),
    }
  }
}
