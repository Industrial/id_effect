# id_effect_i18n

Internationalization for [`id_effect`](../id_effect) services.

0.13 builds this crate in steps (ADR 0010 Amendment 9): Project Fluent message catalogs with CLDR
plural rules, a `CurrentLocale` capability switched per request or fiber, `Accept-Language`
negotiation for `id_effect_axum`, and translated `Schema` validation messages. The first step is
`Locale`, a validated, case-normalized language identifier.

## Quick start

```rust
use id_effect_i18n::Locale;

let locale: Locale = "de_at".parse().unwrap();
assert_eq!(locale.as_str(), "de-AT");
assert_eq!(locale.language_only().as_str(), "de");

// Untrusted input never becomes a path or an unbounded string.
assert!(Locale::parse("../../etc/passwd").is_err());
```

## Secure by default

`Locale::parse` reads at most `Locale::MAX_LEN` bytes and accepts only ASCII subtags of the shapes
`language[-Script][-REGION]` (ADR 0013 R4), so a locale taken from a header or a query parameter is
safe to use as a catalog key.

## Examples

```bash
cargo run -p id_effect_i18n --example 010_locale
```

## Tier

`integration` (ADR 0010 Amendment 9); it moves to `platform` when the Pipeline reference app depends
on it.
