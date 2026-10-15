# Validation and Refinement — Constrained Types

Schemas check structure. Validation adds constraints: an age must be positive, an email must
contain `@`. Refinement goes further: a validated `Email` is a different type from a raw
`String`, so you can never pass an unvalidated string where an email is expected.

## refine / filter: Attach a Predicate

`refine(schema, pred, message)` keeps the values for which `pred` holds and fails with `message`
otherwise. `filter` is the same function under another name.

```rust,ignore
use id_effect::schema::{i64, refine, string};

let age = refine(i64::<()>(), |n| (0..=150).contains(n), "age must be between 0 and 150");
let non_empty = refine(string::<()>(), |s: &String| !s.is_empty(), "must not be empty");

assert!(age.decode(200).is_err());
assert_eq!(age.decode(42), Ok(42));
```

The error has an empty path; an enclosing `struct_` or `array` prefixes it with the field name or
index.

## transform: Fallible Conversion

When conversion can fail (parsing a date, a URL, an email), use `transform(schema, decode,
encode)`. `decode` returns `Result<B, ParseError>`; `encode` maps back to the inner type, so the
schema stays bidirectional:

```rust,ignore
use id_effect::schema::{ParseError, string, transform};

let port = transform(
  string::<()>(),
  |s| s.parse::<u16>().map_err(|e| ParseError::new("", format!("invalid port: {e}"))),
  |p: u16| p.to_string(),
);
assert_eq!(port.decode("8080".into()), Ok(8080));
```

`transform` is also how a `struct_` tuple becomes your own type:

```rust,ignore
let user = transform(
  struct_("name", string::<()>(), "age", i64()),
  |(name, age)| Ok(User { name, age }),
  |u: User| (u.name, u.age),
);
```

## Brand — Nominal Newtypes

A `Brand<A, Tag>` wraps an `A` with a phantom tag, so `Brand<i64, UsdTag>` and `Brand<i64,
EurTag>` are different types with the same runtime representation:

```rust,ignore
use id_effect::Brand;

struct UsdTag;
type Usd = Brand<i64, UsdTag>;

let price = Usd::nominal(100);
assert_eq!(*price.value(), 100);
```

`Brand` keeps `Eq`, `Hash`, `Ord` and `Display` of its inner type, so `Equal` / `EffectHash` still
apply (example `088_brand_equal_hash`). To validate while branding, use `RefinedBrand`:

```rust,ignore
use id_effect::schema::RefinedBrand;

struct EmailTag;
let email = RefinedBrand::<String, EmailTag>::new(|s: &String| {
  if s.contains('@') { Ok(()) } else { Err("invalid email".to_owned()) }
});

assert!(email.try_make("alice@example.com".into()).is_ok());
assert!(email.try_make("alice".into()).is_err());
```

Combine it with `transform` to get a schema that yields the branded type. Functions that need an
email then take `Brand<String, EmailTag>` and won't accept a bare `String`.

## HasSchema — Attaching Schemas to Types

When a type always has the same schema, implement `HasSchema` (or `derive(Schema)`, see the
previous section):

```rust,ignore
use id_effect::schema::{HasSchema, Schema, i64, refine};

struct Age;

impl HasSchema for Age {
  type A = i64;
  type I = i64;
  type E = ();
  fn schema() -> Schema<i64, i64, ()> {
    refine(i64::<()>(), |n| *n >= 0, "age must be non-negative")
  }
}
```

## Summary

| Tool | When to use |
|------|-------------|
| `refine` / `filter` | Predicate on a decoded value |
| `transform` | Fallible conversion, or mapping a tuple to your type |
| `Brand` / `RefinedBrand` | Newtypes that prevent mixing domain values |
| `HasSchema` / `derive(Schema)` | Attach the canonical schema to a type |
