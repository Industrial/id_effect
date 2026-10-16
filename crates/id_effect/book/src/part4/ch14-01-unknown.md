# The Unknown Type — Unvalidated Wire Data

`Unknown` is the type for data that hasn't been validated yet. Think of it as a small,
dependency-free `serde_json::Value`: it can hold any JSON-like shape, but you can't do anything
useful with it until you run it through a schema.

<!-- ignore: excerpt of the id_effect::schema::Unknown definition, not a standalone program -->
```rust,ignore
pub enum Unknown {
  Null,
  Bool(bool),
  I64(i64),
  F64(f64),
  String(String),
  Array(Vec<Unknown>),
  Object(BTreeMap<String, Unknown>),
}
```

## Building Unknown Values

`Unknown` is a plain enum, so you build values with its variants:

```rust
use std::collections::BTreeMap;
use id_effect::schema::Unknown;

let user = Unknown::Object(BTreeMap::from([
  ("name".to_owned(), Unknown::String("Alice".to_owned())),
  ("age".to_owned(), Unknown::I64(30)),
]));
let tags = Unknown::Array(vec![Unknown::String("admin".into()), Unknown::Null]);
```

Config trees, form posts and test fixtures are usually built this way. For JSON, enable the
`schema-serde` feature and convert a `serde_json::Value`:

```rust
use id_effect::schema::{Unknown, unknown_from_serde_json};

let value: serde_json::Value = serde_json::from_str(r#"{"name":"Alice","age":30}"#)?;
let u: Unknown = unknown_from_serde_json(value);
# assert!(matches!(u, Unknown::Object(ref m) if m.get("age") == Some(&Unknown::I64(30))));
# Ok::<(), serde_json::Error>(())
```

Integers become `Unknown::I64`, other numbers `Unknown::F64`, objects a `BTreeMap` (so encoding
order is deterministic). HTTP adapters such as `id_effect_axum` use this bridge instead of their
own conversion code.

## Decoding

Every `Schema` has two decoders:

- `schema.decode(wire)` takes the schema's typed wire value `I` (for `string()` that is a
  `String`, for `struct_(..)` a tuple of the field wires);
- `schema.decode_unknown(&u)` takes an `&Unknown`, checks its shape and reports mismatches with
  a path.

```rust
use id_effect::EffectData;
use id_effect::schema::{Unknown, string};

#[derive(Clone, Debug, EffectData)]
struct T;

let s = string::<T>();
assert!(s.decode_unknown(&Unknown::Null).is_err()); // "expected string, got null"
assert_eq!(s.decode_unknown(&Unknown::String("hi".into())), Ok("hi".to_owned()));
```

(Example `085_unknown_decode`.)

## The Parse Boundary

`Unknown` is your import type. At every I/O boundary (HTTP handler, message consumer, config
file) convert incoming data to `Unknown`, then decode it with a schema. Decoding is synchronous;
lift the result into the effect world with `succeed` / `fail`:

```rust
use id_effect::{Effect, fail, succeed};
use id_effect::schema::{ParseError, Unknown};
# use id_effect::schema::{Schema, string};
# type CreateUser = String;
# #[derive(Debug)]
# enum ApiError { Validation(ParseError) }
# fn create_user_schema() -> Schema<CreateUser, String, ()> { string() }

fn parse_request(raw: &Unknown) -> Effect<CreateUser, ApiError> {
  match create_user_schema().decode_unknown(raw) {
    Ok(req) => succeed(req),
    Err(e) => fail(ApiError::Validation(e)),
  }
}
# assert!(parse_request(&Unknown::String("alice".into())).block_on().into_result().is_ok());
# assert!(parse_request(&Unknown::Null).block_on().into_result().is_err());
```

Nothing beyond the parse boundary sees `Unknown`. Domain functions only accept validated types.
