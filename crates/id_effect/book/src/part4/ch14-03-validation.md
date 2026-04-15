# Validation and Refinement — Constrained Types

Schemas parse structure. Validation adds constraints: an age must be positive, an email must contain `@`, a price must have at most two decimal places. Refinement goes further: a validated `Email` is a different type from a raw `String`, so you can never accidentally pass an unvalidated string where an email is expected.

## refine: Attach a Predicate

`refine` takes a schema and a predicate. Parsing succeeds only if both the schema's parse and the predicate pass:

```rust
use id_effect::schema::{string, i64, refine};

// Age must be between 0 and 150
let age_schema = refine(
    i64(),
    |n| (0..=150).contains(n),
    "age must be between 0 and 150",
);

// Non-empty string
let non_empty = refine(
    string(),
    |s: &String| !s.is_empty(),
    "must not be empty",
);
```

If the predicate returns `false`, parsing fails with a `ParseError` containing the message you provided.

## filter: Same as refine, Different Style

`filter` is an alias for `refine` with a closure-first signature, matching Rust iterator conventions:

```rust
let positive = i64().filter(|n| *n > 0, "must be positive");
let trimmed  = string().filter(|s| s == s.trim(), "must not have leading/trailing whitespace");
```

Use whichever reads more naturally.

## try_map: Fallible Transformation

When conversion logic can fail — parsing a date, constructing a URL, validating an email — use `.try_map`:

```rust
use id_effect::schema::ParseError;

let url_schema = string().try_map(|s| {
    url::Url::parse(&s).map_err(|e| ParseError::custom(format!("invalid URL: {e}")))
});

let date_schema = string().try_map(|s| {
    chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d")
        .map_err(|e| ParseError::custom(format!("invalid date: {e}")))
});
```

`.try_map` runs after the base schema succeeds. The closure returns `Result<NewType, ParseError>`.

## Brand — Newtypes with Zero Cost

A `Brand` is a newtype wrapper that exists only at the type level. At runtime it's transparent. At compile time it prevents mixing up bare primitives with domain values:

```rust
use id_effect::schema::Brand;

// Define branded types
type UserId   = Brand<i64,    UserIdMarker>;
type Email    = Brand<String, EmailMarker>;
type PosPrice = Brand<f64,    PosPriceMarker>;

struct UserIdMarker;
struct EmailMarker;
struct PosPriceMarker;
```

Build schemas that produce branded types:

```rust
let user_id_schema: Schema<UserId> = i64()
    .filter(|n| *n > 0, "user id must be positive")
    .map(Brand::new);

let email_schema: Schema<Email> = string()
    .try_map(|s| {
        if s.contains('@') {
            Ok(Brand::new(s))
        } else {
            Err(ParseError::custom("invalid email"))
        }
    });
```

Now functions that need an `Email` won't compile with a bare `String`:

```rust
fn send_welcome(to: Email) -> Effect<(), MailError, Mailer> { /* … */ }

// This compiles:
send_welcome(parsed_email);

// This doesn't:
send_welcome("alice@example.com".to_string()); // type error: expected Email, found String
```

## HasSchema — Attaching Schemas to Types

When a type always has the same schema, implement `HasSchema`:

```rust
use id_effect::schema::HasSchema;

impl HasSchema for User {
    fn schema() -> Schema<Self> {
        struct_!(User {
            id:    user_id_schema(),
            email: email_schema(),
            name:  non_empty_string_schema(),
        })
    }
}

// Now parse using the impl
let user: User = User::schema().run(raw)?;
```

`HasSchema` types work with generic tooling (exporters, documentation generators, UI scaffolding) that needs to know a type's schema without being parameterised over it.

## Summary

| Tool | When to use |
|------|-------------|
| `refine` / `filter` | Predicate on a successfully-parsed value |
| `try_map` | Fallible conversion after parse |
| `Brand` | Newtypes that prevent mixing domain values |
| `HasSchema` | Attach the canonical schema to a type |
