# `#[derive(SchemaParser)]` and `#[capability]` removed

Both were deprecated no-op/alias macros in 0.6.

```rust
// 0.6                                       // 0.7
#[derive(Clone, SchemaParser)]               #[derive(Clone, Schema)]
struct User { name: String }                 struct User { name: String }

#[id_effect::capability(Db)]                 // delete the attribute: the service type is the
struct DbKey;                                // key — caps!(Db), Needs<Db>, #[provides(Db)]
```

Text parsers: `id_effect_parse::SchemaBridge::parser_for::<User>()`.
