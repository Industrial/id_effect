# Removed: `id_effect_tower` and `id_effect_sql_pg`

The 0.6 re-export shims are gone. Depend on their new homes instead.

```toml
# before (0.6)
id_effect_tower = "0.6"
id_effect_sql_pg = "0.6"
# after (0.7)
id_effect_axum = { version = "0.7", features = ["tower"] }
id_effect_sql = { version = "0.7", features = ["pg"] }
```

Paths: `id_effect_tower::*` becomes `id_effect_axum::service::*`; `id_effect_sql_pg::*` becomes
`id_effect_sql::pg::*`.
