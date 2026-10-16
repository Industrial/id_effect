# Schema Combinators — Describing Data Shapes

A schema is a value that describes how to decode wire data into a typed result and how to encode
it back. Schemas compose: build small schemas for primitives, then combine them into schemas for
structures. All combinators live in `id_effect::schema` and are plain functions.

## Primitive Schemas

```rust,ignore
use id_effect::schema::{Schema, bool_, f64, i64, string};

let name: Schema<String, String, ()> = string();
let age: Schema<i64, i64, ()> = i64();
let price: Schema<f64, f64, ()> = f64(); // decode_unknown also accepts Unknown::I64
let active: Schema<bool, bool, ()> = bool_();
```

Each primitive's wire type is the value itself; `decode_unknown` checks the `Unknown` variant.
The third type parameter is the phantom tag `E`; annotate it once (`i64::<()>()`) when nothing
else fixes it.

## Structs and Tuples

`struct_` takes field names and field schemas and decodes to a tuple. `struct3` … `struct12`
cover wider shapes; `tuple` … `tuple12` do the same for positional arrays.

```rust,ignore
use id_effect::schema::{i64, string, struct_};

let person = struct_("name", string::<()>(), "age", i64());
// decode_unknown reads an Unknown::Object; a missing key fails with path "age",
// message "missing field age".
let (name, age): (String, i64) = person.decode_unknown(&raw)?;
```

To decode into your own type, map the tuple with `transform` (see the next section), or derive
the schema:

```rust,ignore
#[derive(Clone, Debug, PartialEq, id_effect::Schema)]
struct User {
  #[schema("user_name")] // wire key
  name: String,
  age: i64,
  nick: Option<String>,
  tags: Vec<String>,
  address: Address, // any type that itself derives Schema
}

let wire: Unknown = User::schema().encode(user.clone()); // Unknown::Object
assert_eq!(User::schema().decode_unknown(&wire), Ok(user));
```

`derive(Schema)` implements `HasSchema` with wire type `Unknown`. It accepts `String`, `bool`,
`i64`, `i32`, `f64`, nested `HasSchema` types, and `Option` / `Vec` of those, with any number of
fields. Errors carry the field name as their path.

## Optional and Nullable Values

```rust,ignore
use id_effect::schema::{i64, null_or, optional};

let port = optional(i64::<()>()); // Schema<Option<i64>, Option<i64>, ()>
let parent = null_or(i64::<()>());
```

Both decode `Unknown::Null` as `None`. Inside `struct_` a field must still be present; use
`derive(Schema)` with an `Option<T>` field when the key may be missing.

## Arrays and Records

```rust,ignore
use id_effect::schema::{array, i64, record, string};

let tags = array(string::<()>()); // Vec<String>; an error at index 2 has path "2"
let scores = record(i64::<()>()); // BTreeMap<String, i64>; errors are prefixed with the key
```

## Literals and Unions

```rust,ignore
use id_effect::schema::{i64_unknown_wire, literal_string, union_, union_chain};

let kind = literal_string::<()>("user"); // only the exact string "user"

// Try schemas in order; the first successful decode wins. Wire type is Unknown.
let id = union_(i64_unknown_wire::<()>(), id_from_string());
let any = union_chain(vec![a, b, c]);
```

`union_` / `union_chain` encode with the **first** schema, so the arms should agree on
encoding. For recursive shapes, `suspend(|| schema())` defers building the inner schema;
`suspend_named("Node", schema)` does the same and gives the recursion a name, which JSON Schema
export uses for `$defs` / `$ref` (see below).

## Running a Schema

```rust,ignore
let wire = person.encode(("Alice".to_owned(), 30));
assert_eq!(person.decode(wire), Ok(("Alice".to_owned(), 30)));
```

`decode`, `decode_unknown` and `encode` are synchronous. The round-trip law
`decode(encode(a)) == a` holds for every combinator and is property-tested in
`crates/id_effect/tests/schema_roundtrip.rs`.

## Schema as a Type Contract

A schema is documentation: where a handler takes a `Schema<CreateUser, Unknown, ()>`, readers
know exactly which shape of data it accepts, checked at runtime. Implement `HasSchema` (or derive
it) so generic tooling, such as the JSON Schema export behind the `schema-serde` feature, can
find a type's canonical schema.

## Exporting JSON Schema

Every combinator also records a description of the wire shape it accepts, a `SchemaAst`
(`schema.ast()`). With the `schema-serde` feature, `schema.json_schema()`,
`to_json_schema(&schema)` or `json_schema_for::<T: HasSchema>()` turn it into a JSON Schema
**draft 2020-12** document. Because the exporter reads the *same* schema value that decodes,
the published contract cannot drift from the validation:

```rust,ignore
use id_effect::schema::{email, i64, range, string, struct_};

let signup = struct_(
  "email", email(string::<()>()).describe("Login address"),
  "age", range(i64::<()>(), 13, 130),
)
.title("Signup");

assert_eq!(signup.json_schema(), serde_json::json!({
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "Signup",
  "type": "object",
  "properties": {
    "email": { "type": "string", "format": "email", "description": "Login address" },
    "age": { "type": "integer", "minimum": 13, "maximum": 130 }
  },
  "required": ["email", "age"]
}));
```

| Schema | JSON Schema |
|--------|-------------|
| `string` / `i64` / `f64` / `bool_` | `type` |
| `literal_string` / `literal_i64` | `const` |
| `array` / `record` | `items` / `additionalProperties` |
| `tuple*` | `prefixItems`, `items: false` |
| `struct_*` | `properties`, `required` |
| `optional` / `null_or` / `union_` / `union_chain` | `anyOf` |
| `suspend_named` | `$ref` into `$defs` |
| built-in validators ([next section](./ch14-03-validation.md)) | `minLength`, `pattern`, `format`, `enum`, `minimum`, … |
| `.title` / `.describe` / `.example` / `.default_value` / `.deprecated` | `title`, `description`, `examples`, `default`, `deprecated` |

What cannot be introspected is exported loosely rather than wrongly: a hand-written
`Schema::make` codec exports `{}` (accept anything) unless you attach a description with
`.with_ast(...)`, an arbitrary `refine` predicate adds no keyword, and `transform` exports its
inner wire schema. `#[derive(Schema)]` types currently build through `Schema::make`, so they
export as `{}` for now. OpenAPI documents generated from routes are a later release.
