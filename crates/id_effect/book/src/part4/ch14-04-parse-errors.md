# ParseError and ParseErrors — Path-Aware Failures

A failed decode returns a `ParseError`: a dot-separated `path` to the offending value and a
human-readable `message`. `ParseErrors` collects several of them when you validate more than one
thing and want to report everything at once.

## ParseError

```rust,ignore
pub struct ParseError {
  pub path: String,    // "" at the root, "user.age", "items.2", …
  pub message: String,
}
```

Build one with `ParseError::new(path, message)`. Combinators prefix paths as they descend:
`struct_` / `struct3..` / `derive(Schema)` add the field name, `array` and `tupleN` add the index,
`record` adds the key. You can do the same in custom code:

```rust,ignore
use id_effect::schema::ParseError;

let e = ParseError::new("age", "bad").prefix("user");
assert_eq!(e.path, "user.age");
let e = ParseError::new("", "expected string").prefix_index(2);
assert_eq!(e.path, "2");
```

(Example `086_parse_error_paths`.)

A schema's `decode` stops at the first failure: decoding a struct whose `name` and `age` are
both wrong reports the first field in declaration order.

## ParseErrors: Collecting Several Issues

`ParseErrors { issues: Vec<ParseError> }` is the type to return when you check several
independent inputs and want to show all problems at once, for example every field of a form:

```rust,ignore
use id_effect::schema::{ParseError, ParseErrors};

fn validate_form(form: &Unknown) -> Result<Form, ParseErrors> {
  let name = name_schema().decode_unknown(field(form, "name")).map_err(|e| e.prefix("name"));
  let age = age_schema().decode_unknown(field(form, "age")).map_err(|e| e.prefix("age"));
  match (name, age) {
    (Ok(name), Ok(age)) => Ok(Form { name, age }),
    (n, a) => Err(ParseErrors::new(
      [n.err(), a.err()].into_iter().flatten().collect(),
    )),
  }
}
```

`ParseErrors::one(e)` wraps a single error. `Display` prints one `path: message` line per issue
(just the message when the path is empty); `ParseErrors` implements `std::error::Error`.

## At the Effect Boundary

Decoding returns a plain `Result`. Lift it into an `Effect` with `succeed` / `fail`, mapping the
error into your domain error type:

```rust,ignore
use id_effect::{Effect, fail, succeed};

#[derive(Debug)]
enum ApiError {
  Validation(Vec<(String, String)>),
}

fn to_api_error(errs: ParseErrors) -> ApiError {
  ApiError::Validation(errs.issues.into_iter().map(|e| (e.path, e.message)).collect())
}

fn parse_form(raw: Unknown) -> Effect<Form, ApiError> {
  match validate_form(&raw) {
    Ok(form) => succeed(form),
    Err(errs) => fail(to_api_error(errs)),
  }
}
```

## Summary

- `ParseError` = one failure with a `path` and a `message`
- combinators prefix paths automatically; `decode` stops at the first failure
- `ParseErrors` = several failures you collected yourself, displayed one per line
- convert to your API error type at the boundary and keep the paths
