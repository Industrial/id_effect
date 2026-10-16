# `id_effect_authz`

Authorization for id_effect services.

**Status: in development for 0.9.** This crate is registered (workspace member, publish order,
CI) but has no public API yet; its first crates.io release carries that API
(ADR 0010 Amendment 5). Planned scope:

- role and permission sets, named policies, resource/ownership and attribute policies
- route guards and an allow/deny event stream
- test helpers that assert a policy allows or denies

```rust
// The crate links; its API lands during the 0.9 release.
use id_effect_authz as _;
```

Tier: `integration` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md)).
