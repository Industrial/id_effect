# Schema Combinators — Describing Data Shapes

A schema is a value that describes how to parse an `Unknown` into a typed result. Schemas compose: build small schemas for primitive types, then combine them into schemas for complex structures.

## Primitive Schemas

```rust
use id_effect::schema::{string, integer, i64, f64, boolean, null};

// Parse a string
let name_schema = string();

// Parse an integer (i64)
let age_schema = i64();

// Parse a float
let price_schema = f64();

// Parse a boolean
let active_schema = boolean();
```

Each schema has type `Schema<T>` — `string()` is a `Schema<String>`, `i64()` is a `Schema<i64>`, and so on.

## Struct Schemas

```rust
use id_effect::schema::struct_;

#[derive(Debug)]
struct User {
    name: String,
    age:  i64,
}

let user_schema = struct_!(User {
    name: string(),
    age:  i64(),
});
```

`struct_!` maps field names to their schemas and constructs the target type. If any field is missing or has the wrong type, parsing fails with a `ParseError` that includes the field path.

For schemas without a derive macro, use `object`:

```rust
use id_effect::schema::object;

let user_schema = object([
    ("name", string().map(|s| s)),
    ("age",  i64()),
]).map(|(name, age)| User { name, age });
```

## Optional Fields

```rust
use id_effect::schema::optional;

struct Config {
    host:    String,
    port:    Option<u16>,
    timeout: Option<Duration>,
}

let config_schema = struct_!(Config {
    host:    string(),
    port:    optional(u16()),
    timeout: optional(duration_ms()),
});
```

`optional(schema)` produces `Schema<Option<T>>`. A missing field or `null` both parse as `None`.

## Array Schemas

```rust
use id_effect::schema::array;

// Vec of strings
let tags_schema: Schema<Vec<String>> = array(string());

// Vec of User
let users_schema: Schema<Vec<User>> = array(user_schema);
```

`array(item_schema)` parses a JSON array where each element is validated by `item_schema`. Errors include the index: `"[2].email: expected string, got null"`.

## Union Schemas

```rust
use id_effect::schema::{union_, literal_string};

#[derive(Debug)]
enum Status { Active, Inactive, Pending }

let status_schema = union_![
    literal_string("active")   => Status::Active,
    literal_string("inactive") => Status::Inactive,
    literal_string("pending")  => Status::Pending,
];
```

`union_!` tries each branch in order and returns the first that succeeds. Errors report all branches that failed.

## Transforming Schemas

Schemas are values — you can `.map` them:

```rust
// Parse a string and convert it to uppercase
let upper_schema: Schema<String> = string().map(|s| s.to_uppercase());

// Parse a string and try to convert to a domain type
let email_schema: Schema<Email> = string().try_map(|s| {
    Email::parse(s).map_err(ParseError::custom)
});
```

`.map` transforms on success. `.try_map` can fail and produce a `ParseError`.

## Running a Schema

```rust
use id_effect::schema::parse;

let raw: Unknown = Unknown::from_json_str(r#"{"name":"Alice","age":30}"#)?;

match parse(user_schema, raw) {
    Ok(user)  => println!("Got: {user:?}"),
    Err(errs) => println!("Errors: {errs}"),
}
```

`parse` returns `Result<T, ParseErrors>`. `ParseErrors` accumulates all errors — not just the first — so a caller gets the complete picture of what's wrong.

## Schema as a Type Contract

A schema is documentation. Where you use `Schema<CreateUserRequest>`, readers know: this function requires exactly this shape of data, checked at runtime. The schema is the spec.

```rust
pub fn create_user_handler() -> impl Fn(Unknown) -> Effect<User, ApiError, Db> {
    let schema = create_user_schema();
    move |raw| {
        effect! {
            let req = parse(schema.clone(), raw)
                .map_err(ApiError::Validation)?;
            ~ db_create_user(req)
        }
    }
}
```
