# id_effect_i18n

Internationalization for [`id_effect`](../id_effect) services, over
[Project Fluent](https://projectfluent.org).

- **`Catalog`**: Fluent resources per locale, embedded with `include_str!` or loaded from
  allowlisted directories, parsed once at startup. Lookups fall back from the requested locale to
  its language and then to the default locale; `Catalog::check` reports every key a locale lacks.
- **CLDR plurals** through Fluent selectors (`{ $n -> [one] … [few] … *[other] … }`), with the
  rules of the locale a message comes from.
- **`CurrentLocale`**: the locale capability in the environment, switched per request or per fiber
  with `with_locale`; `translate` formats a message in it.
- **`Locale`**: a validated, case-normalized language identifier (`de_at` → `de-AT`).

## Quick start

```rust
use id_effect::{Effect, Exit, HCons, HNil, Never, hlist, run_test};
use id_effect_i18n::{Args, Catalog, CurrentLocale, Locale, translate, with_locale};

let en: Locale = "en".parse().unwrap();
let de: Locale = "de".parse().unwrap();
let catalog = Catalog::builder(en.clone())
    .embedded(en.clone(), "en/app.ftl", "inbox = { $n ->
        [one] one message
       *[other] { $n } messages
    }")
    .embedded(de.clone(), "de/app.ftl", "inbox = { $n ->
        [one] eine Nachricht
       *[other] { $n } Nachrichten
    }")
    .use_isolating(false)
    .build()
    .expect("every resource parses"); // a syntax error fails startup
assert!(catalog.check().is_complete());

// Per request: switch the locale for the handler's effect (and every fiber it forks).
type Env = HCons<Catalog, HCons<CurrentLocale, HNil>>;
let handler: Effect<String, Never, Env> =
    with_locale("de-AT".parse().unwrap(), translate("inbox", Args::new().with("n", 2)));
let exit = run_test(handler.provide_all(hlist![catalog, CurrentLocale::new(en)]));
assert!(matches!(exit, Exit::Success(text) if text == "2 Nachrichten")); // de-AT → de

// Untrusted input never becomes a path or an unbounded string.
assert!(Locale::parse("../../etc/passwd").is_err());
```

## Loading from a directory

```text
locales/
  en/app.ftl   en/errors.ftl
  de/app.ftl
  pt-BR/app.ftl
```

```rust,no_run
use id_effect_i18n::{Catalog, Locale};

let catalog = Catalog::builder(Locale::parse("en").unwrap())
    .allow_dir("locales")?   // only files below here are read (`..` and symlinks resolved)
    .dir("locales")?         // every <locale>/*.ftl
    .require_complete()      // optional: a missing key fails startup too
    .build()?;
# Ok::<(), id_effect_i18n::CatalogError>(())
```

## Secure by default

`Locale::parse` reads at most `Locale::MAX_LEN` bytes and accepts only ASCII subtags of the shapes
`language[-Script][-REGION]` (ADR 0013 R4), and catalogs are keyed by parsed locales, so a locale
taken from a header or a query parameter never reaches the file system. Resources are read once at
startup, only `.ftl` files of at most 1 MiB below allowlisted directories; parse errors and
duplicate messages fail `build`. Arguments are wrapped in Unicode bidi isolation marks by default.

## Features

All off by default; filled in by the 0.13 web step: `axum` (Accept-Language negotiation and a
layer providing `CurrentLocale`), `maud` (rendering messages into HTML), `schema` (translated
`Schema` validation messages).

## Examples

```bash
cargo run -p id_effect_i18n --example 010_locale
cargo run -p id_effect_i18n --example 020_catalog
```

## Tier

`integration` (ADR 0010 Amendment 9); it moves to `platform` when the Pipeline reference app depends
on it.
