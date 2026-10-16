# `id_effect_auth`

Authentication for id_effect services.

**Status: in development for 0.9.** This crate is registered (workspace member, publish order,
CI) but has no public API yet; its first crates.io release carries that API
(ADR 0010 Amendment 5). Planned scope:

- password hashing (argon2id) and persistent sessions
- bearer/JWT (with JWKS rotation), basic and API-key credential layers producing a principal
- OIDC authorization code + PKCE, TOTP second factor

```rust
// The crate links; its API lands during the 0.9 release.
use id_effect_auth as _;
```

Tier: `integration` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md)).
