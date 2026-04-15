# The Unknown Type — Unvalidated Wire Data

`Unknown` is the type for data that hasn't been validated yet. Think of it as a typed `serde_json::Value` — it can hold any shape of data, but you can't do anything useful with it until you run it through a schema.

## Creating Unknown Values

```rust
use id_effect::schema::Unknown;

// From a JSON string
let u: Unknown = Unknown::from_json_str(r#"{"name": "Alice", "age": 30}"#)?;

// From a serde_json Value
let v: serde_json::Value = serde_json::json!({ "name": "Alice" });
let u: Unknown = Unknown::from_serde_json(v);

// From raw parts
let u: Unknown = Unknown::object([
    ("name", Unknown::string("Alice")),
    ("age",  Unknown::integer(30)),
]);

// Primitives
let s: Unknown = Unknown::string("hello");
let n: Unknown = Unknown::integer(42);
let b: Unknown = Unknown::boolean(true);
let null: Unknown = Unknown::null();
let arr: Unknown = Unknown::array([Unknown::integer(1), Unknown::integer(2)]);
```

## Why Not serde_json::Value Directly?

`serde_json::Value` is an excellent data type, but it's stringly typed: `value["name"]` gives you an `Option<&Value>` and there's no structure around parse errors, path tracking, or accumulation. `Unknown` wraps the same idea but integrates with id_effect's schema parser, which gives you:

- **Path tracking** — "error at `.users[3].email`"
- **Accumulated errors** — all failures in one parse, not just the first
- **Composable schemas** — build complex validators from simple primitives

## Inspecting Unknown Values

You don't normally inspect `Unknown` directly — you run it through a schema. But when debugging:

```rust
// Check what shape the value has
match u.kind() {
    UnknownKind::Object(fields) => { /* … */ }
    UnknownKind::Array(elems)   => { /* … */ }
    UnknownKind::String(s)      => { /* … */ }
    UnknownKind::Integer(n)     => { /* … */ }
    UnknownKind::Float(f)       => { /* … */ }
    UnknownKind::Boolean(b)     => { /* … */ }
    UnknownKind::Null            => { /* … */ }
}

// Access a field without parsing (returns Option<&Unknown>)
let name: Option<&Unknown> = u.field("name");
```

## The Parse Boundary

`Unknown` is your import type. At every IO boundary — HTTP handler, NATS message, config file, database row — convert incoming data to `Unknown` first, then parse it with a schema:

```rust
async fn handle_request(body: Bytes) -> Effect<CreateUserResponse, ApiError, Deps> {
    effect! {
        // Convert raw bytes to Unknown
        let raw = Unknown::from_json_bytes(&body)
            .map_err(ApiError::InvalidJson)?;

        // Parse Unknown into a typed, validated struct
        let req: CreateUserRequest = ~ parse_schema(create_user_schema(), raw);

        // Now req is fully trusted — proceed with domain logic
        ~ create_user(req)
    }
}
```

Nothing beyond the parse boundary sees `Unknown`. Domain functions only accept validated types.

## Unknown and Serde

If you have existing `serde`-deserializable types, use the serde bridge (requires the `schema-serde` feature):

```rust
use id_effect::schema::serde_bridge::unknown_from_serde_json;

// Deserialise via serde, then convert to Unknown for schema validation
let value: serde_json::Value = serde_json::from_str(input)?;
let u: Unknown = unknown_from_serde_json(value);
```

This lets you incrementally adopt the schema system without rewriting all your serde impls at once.
