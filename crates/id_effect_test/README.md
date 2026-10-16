# `id_effect_test`

Integration-test support for id_effect programs.

**Status: in development for 0.9.** This crate is registered (workspace member, publish order,
CI) but has no public API yet; its first crates.io release carries that API
(ADR 0010 Amendment 5). Planned scope:

- effect-native containers (Postgres, MySQL, Redis, Keycloak, Vault) as scoped layers
- per-test database isolation (rolled-back transaction or template-database clone)
- fixtures/factories and the in-process HTTP client

```rust
// The crate links; its API lands during the 0.9 release.
use id_effect_test as _;
```

Tier: `integration` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md)).
