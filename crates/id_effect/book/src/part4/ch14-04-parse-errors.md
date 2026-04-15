# ParseErrors — Structured Error Accumulation

When a user submits a form with five invalid fields, they deserve to know about all five — not just the first one you found. `ParseErrors` is id_effect's solution: errors accumulate across an entire parse, and you report them all at once.

## ParseError vs ParseErrors

```rust
use id_effect::schema::{ParseError, ParseErrors};

// One error
let e: ParseError = ParseError::custom("age must be positive");

// Many errors
let es: ParseErrors = ParseErrors::single(e);
```

`ParseError` is a single failure. `ParseErrors` is a non-empty collection of failures with path information.

## What a ParseError Contains

```rust
// A parse error has:
// - a message
// - a path (where in the data structure it occurred)
// - optionally, the value that failed

let err = ParseError::builder()
    .message("expected integer, got string")
    .path(["users", "0", "age"])
    .received(Unknown::string("thirty"))
    .build();

println!("{err}");
// → users[0].age: expected integer, got string (received: "thirty")
```

## Path Tracking

Paths are built automatically as schemas descend into nested structures. You don't need to set them manually:

```rust
let raw = Unknown::from_json_str(r#"
  {
    "users": [
      { "name": "Alice", "age": 30 },
      { "name": "Bob",   "age": "thirty" }
    ]
  }
"#)?;

let result = parse(users_schema, raw);
// Err(ParseErrors {
//   errors: [
//     ParseError { path: "users[1].age", message: "expected integer" }
//   ]
// })
```

The `struct_!` macro and `array` combinator push path segments automatically. Custom schemas using `.try_map` or `.filter` inherit the current path.

## Accumulation

The key property of `ParseErrors` is accumulation. When parsing a struct with multiple fields, failures from different fields are collected, not short-circuited:

```rust
let raw = Unknown::from_json_str(r#"
  { "name": "", "age": -5, "email": "not-an-email" }
"#)?;

let result: Result<User, ParseErrors> = parse(user_schema, raw);
// Err(ParseErrors {
//   errors: [
//     ParseError { path: "name",  message: "must not be empty" },
//     ParseError { path: "age",   message: "age must be between 0 and 150" },
//     ParseError { path: "email", message: "invalid email" },
//   ]
// })
```

All three errors reported in one call. No round-trips.

## Using ParseErrors at API Boundaries

Convert `ParseErrors` to your API's error type:

```rust
#[derive(Debug)]
enum ApiError {
    Validation(Vec<FieldError>),
    Internal(String),
}

#[derive(Debug)]
struct FieldError {
    field:   String,
    message: String,
}

fn to_api_errors(errs: ParseErrors) -> ApiError {
    ApiError::Validation(
        errs.into_iter()
            .map(|e| FieldError {
                field:   e.path().to_string(),
                message: e.message().to_string(),
            })
            .collect()
    )
}
```

## ParseErrors in Effects

`parse` returns a plain `Result`. To lift into an `Effect`:

```rust
effect! {
    let raw = Unknown::from_json_bytes(&body)
        .map_err(ApiError::InvalidJson)?;

    let req = parse(create_user_schema(), raw)
        .map_err(to_api_errors)?;

    ~ create_user(req)
}
```

The `?` operator on a `Result<T, ParseErrors>` inside `effect!` maps the error into `E` via `From`. Define `impl From<ParseErrors> for YourError` to make this ergonomic.

## Displaying ParseErrors

`ParseErrors` implements `Display` with a human-readable multiline format:

```
Validation failed (3 errors):
  name: must not be empty
  age: age must be between 0 and 150
  email: invalid email
```

And `Debug` for the raw structure when inspecting in tests.

## Summary

- `ParseError` = one failure with a message and a path
- `ParseErrors` = all failures from a complete parse attempt
- Paths are tracked automatically by schema combinators
- Accumulation means the user sees all problems at once
- Convert to your API error type at the boundary; keep the path information
