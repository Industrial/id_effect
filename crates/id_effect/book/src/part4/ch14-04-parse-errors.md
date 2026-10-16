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

A schema's `decode` and `decode_unknown` stop at the first failure: decoding a struct whose
`name` and `age` are both wrong reports the first field in declaration order.

## decode_unknown_all: Every Failure in One Pass

For user-facing input you usually want every problem at once. `decode_unknown_all` walks the
whole input and returns `Result<A, ParseErrors>`: structs keep going after a bad or missing
field, arrays after a bad element, records after a bad value, tuples after a bad position, at
every nesting level.

```rust,ignore
use id_effect::schema::{email, i64, length, range, string, struct3};

let signup = struct3(
  "name", length(string::<()>(), 2, 20),
  "email", email(string::<()>()),
  "age", range(i64::<()>(), 0, 150),
);

// {"name": "x", "email": "nope"}
let errs = signup.decode_unknown_all(&input).unwrap_err();
// name: must be at least 2 characters
// email: must be a valid email address
// age: missing field age
println!("{errs}");
```

The two paths agree: on success `decode_unknown_all` returns what `decode_unknown` returns, and on
failure its first issue is exactly the `decode_unknown` error. A single leaf (a refinement chain
on one value) still reports one issue. `decode` on the typed wire (`I`) stays fail-fast.

## ParseErrors: Collecting Several Issues

`ParseErrors { issues: Vec<ParseError> }` is what `decode_unknown_all` returns. You can also
build one yourself when you check several independent inputs and want to show all problems at
once:

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

`ParseErrors::one(e)` wraps a single error; `prefix` / `prefix_index` re-root every issue.
`Display` prints one `path: message` line per issue (just the message when the path is empty);
`ParseErrors` implements `std::error::Error`.

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
- combinators prefix paths automatically; `decode` / `decode_unknown` stop at the first failure
- `decode_unknown_all` reports every field / element failure as `ParseErrors`
- `ParseErrors` = several failures, displayed one per line
- convert to your API error type at the boundary and keep the paths
