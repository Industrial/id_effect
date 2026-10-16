# Validation and Refinement — Constrained Types

Schemas check structure. Validation adds constraints: an age must be positive, an email must
contain `@`. Refinement goes further: a validated `Email` is a different type from a raw
`String`, so you can never pass an unvalidated string where an email is expected.

## refine / filter: Attach a Predicate

`refine(schema, pred, message)` keeps the values for which `pred` holds and fails with `message`
otherwise. `filter` is the same function under another name.

```rust
use id_effect::schema::{i64, refine, string};

let age = refine(i64::<()>(), |n| (0..=150).contains(n), "age must be between 0 and 150");
let non_empty = refine(string::<()>(), |s: &String| !s.is_empty(), "must not be empty");

assert!(age.decode(200).is_err());
assert_eq!(age.decode(42), Ok(42));
```

The error has an empty path; an enclosing `struct_` or `array` prefixes it with the field name or
index.

## Built-in Validators

The common checks ship ready-made. Each is a `filter` that also records a declarative constraint,
so it shows up in JSON Schema export ([ch14-02](./ch14-02-combinators.md#exporting-json-schema)):

| Validator | Applies to | Error message | JSON Schema |
|-----------|-----------|---------------|-------------|
| `min_length(s, n)` / `max_length(s, n)` / `length(s, min, max)` / `non_empty(s)` | `String` (counts `chars()`) | `must be at least n characters`, … | `minLength` / `maxLength` |
| `pattern(s, source, matcher)` | `String` | `must match pattern <source>` | `pattern` |
| `regex(s, source)?` (feature `schema-regex`) | `String` | `must match pattern <source>` | `pattern` |
| `email(s)` | `String` | `must be a valid email address` | `format: email` |
| `uuid(s)` | `String` | `must be a valid UUID` | `format: uuid` |
| `url(s)` | `String` | `must be a valid URL` | `format: uri` |
| `one_of(s, &[..])` | `String` | `must be one of: a, b` | `enum` |
| `minimum` / `maximum` / `range(s, min, max)` | `i64`, `f64` | `must be >= min`, `must be <= max` | `minimum` / `maximum` |
| `exclusive_minimum` / `exclusive_maximum` | `i64`, `f64` | `must be > min`, `must be < max` | `exclusiveMinimum` / `exclusiveMaximum` |
| `min_items` / `max_items` | `Vec<A>` | `must have at least n items`, … | `minItems` / `maxItems` |

```rust
use id_effect::schema::{email, i64, length, range, string};

let name = length(string::<()>(), 1, 40);
let contact = email(string::<()>());
let age = range(i64::<()>(), 0, 150);
# assert!(name.decode(String::new()).is_err());
# assert!(contact.decode("alice@example.com".to_owned()).is_ok());
# assert!(age.decode(151).is_err());
```

`email` and `url` are pragmatic syntax checks, not full RFC parsers.

For regular expressions, enable the `schema-regex` feature and use `regex`. It compiles the
expression with the `regex` crate, validates with it, and exports the **same** string as JSON
Schema `pattern`, so the check and the contract cannot drift. It returns `Err(regex::Error)` when
the expression does not compile:

```rust
use id_effect::schema::{regex, string};

let slug = regex(string::<()>(), r"^[a-z0-9]+(-[a-z0-9]+)*$")?;
assert_eq!(slug.json_schema()["pattern"], "^[a-z0-9]+(-[a-z0-9]+)*$"); // `schema-serde`
# assert!(slug.decode("not a slug".to_owned()).is_err());
# Ok::<(), Box<dyn std::error::Error>>(())
```

Like JSON Schema, matching is unanchored: write `^...$` to match the whole string. JSON Schema
patterns are ECMA-262; for a portable contract, stay in the syntax both dialects share (classes,
quantifiers, groups, alternation, anchors). Rust-only syntax such as `(?P<name>...)` or `\z`
validates here, but other JSON Schema tools may reject it.

The feature is optional so crates that never validate patterns do not compile a regex engine.
Without it, `pattern` takes the expression to publish plus the matcher that enforces it, for
example `pattern(string(), r"^[a-z]+$", move |s| re.is_match(s))`. Keep the two in sync.

## transform: Fallible Conversion

When conversion can fail (parsing a date, a URL, an email), use `transform(schema, decode,
encode)`. `decode` returns `Result<B, ParseError>`; `encode` maps back to the inner type, so the
schema stays bidirectional:

```rust
use id_effect::schema::{ParseError, string, transform};

let port = transform(
  string::<()>(),
  |s| s.parse::<u16>().map_err(|e| ParseError::new("", format!("invalid port: {e}"))),
  |p: u16| p.to_string(),
);
assert_eq!(port.decode("8080".into()), Ok(8080));
```

`transform` is also how a `struct_` tuple becomes your own type:

```rust
# use id_effect::schema::{i64, string, struct_, transform};
# #[derive(Debug, PartialEq)]
# struct User { name: String, age: i64 }
let user = transform(
  struct_("name", string::<()>(), "age", i64()),
  |(name, age)| Ok(User { name, age }),
  |u: User| (u.name, u.age),
);
# assert_eq!(user.decode(("Ada".to_owned(), 36)), Ok(User { name: "Ada".to_owned(), age: 36 }));
```

## Brand — Nominal Newtypes

A `Brand<A, Tag>` wraps an `A` with a phantom tag, so `Brand<i64, UsdTag>` and `Brand<i64,
EurTag>` are different types with the same runtime representation:

```rust
use id_effect::Brand;

struct UsdTag;
type Usd = Brand<i64, UsdTag>;

let price = Usd::nominal(100);
assert_eq!(*price.value(), 100);
```

`Brand` keeps `Eq`, `Hash`, `Ord` and `Display` of its inner type, so `Equal` / `EffectHash` still
apply (example `088_brand_equal_hash`). To validate while branding, use `RefinedBrand`:

```rust
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

```rust
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
# assert!(Age::schema().decode(-1).is_err());
```

## Summary

| Tool | When to use |
|------|-------------|
| `refine` / `filter` | Predicate on a decoded value |
| `email`, `uuid`, `url`, `length`, `range`, `pattern`, `one_of`, … | Common checks that also appear in exported JSON Schema |
| `transform` | Fallible conversion, or mapping a tuple to your type |
| `Brand` / `RefinedBrand` | Newtypes that prevent mixing domain values |
| `HasSchema` / `derive(Schema)` | Attach the canonical schema to a type |
