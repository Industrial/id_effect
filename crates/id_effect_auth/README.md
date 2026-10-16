# `id_effect_auth`

Authentication for id_effect services.

- **Passwords** (`password`): `PasswordHasher` produces argon2id PHC strings with the OWASP
  parameters by default (19 MiB, 2 iterations, 1 lane). `verify` hands back an upgraded hash
  when the stored one used older parameters, and `verify_or_dummy` checks unknown users against
  a dummy hash so their response time matches a wrong password's. Hashing runs on the
  executor's blocking pool.
- **Sessions** (`session`): `Sessions` over a `SessionStore` (in-memory `MemorySessionStore`
  for tests), with idle and absolute timeouts, a fresh id on every login (session fixation),
  rotation, logout and revoke-all, and a `__Host-` / `Secure` / `HttpOnly` / `SameSite=Lax`
  cookie. Feature `pg` adds `PgSessionStore`: sessions in PostgreSQL over `id_effect_sql`'s
  `SqlClient`, keyed by the SHA-256 of the id (never the id), with an upsert `save` and the
  schema as a migration (`migrations/`) or `ensure_schema()`. Feature `axum` adds
  `SessionLayer`, which loads the request's session into `id_effect_axum`'s request
  capabilities.
- **Principals** (`principal`): `AuthPrincipal` implements `id_effect_authz::Principal` (id,
  roles, permissions, claims, tenant) and records the `AuthScheme` that authenticated it.
- **API keys** (`apikey`): `<prefix>_<id>_<secret>` keys with 256-bit secrets, stored as SHA-256
  digests, found by public id and compared in constant time (`ApiKeys`, `ApiKeyStore`).
- **Basic** (`basic`): `BasicAuth` checks a username and password against a `UserLookup` with
  `PasswordHasher::verify_or_dummy` and stores upgraded hashes.
- **JWT** (feature `jwt`, jsonwebtoken): `JwtVerifier` checks HS/RS/PS/ES/EdDSA signatures,
  `iss`, `aud`, `exp` / `nbf` with leeway and required claims, and maps claims to a principal
  (`ClaimMapping`, dotted paths such as `realm_access.roles`); `JwtIssuer` signs first-party
  tokens and publishes its public JWK. Feature `jwks` adds `JwksClient`, which fetches and caches
  an identity provider's key set and refetches it, rate-limited, when a token names an unknown
  `kid` (key rotation).
- **Middleware** (feature `axum`): `Authenticate` tries session → bearer → API key → Basic (the
  order is configurable) and installs the principal as `id_effect_authz`'s `CurrentPrincipal`.
  No credential continues anonymously (the route guard decides); an invalid one answers 401
  Problem Details with `WWW-Authenticate` challenges.

```rust
use id_effect::Exit;
use id_effect_auth::{
  MemorySessionStore, PasswordHasher, PasswordParams, SessionConfig, Sessions, Verification,
};

// Production code uses `PasswordHasher::default()` (OWASP parameters); low cost keeps this fast.
let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap());
let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());

let Exit::Success(stored) = hasher.hash("s3cret").block_on() else { panic!() };
let Exit::Success(check) = hasher.verify_or_dummy("s3cret", Some(&stored)).block_on() else {
  panic!()
};
assert_eq!(check, Verification::Valid);

let Exit::Success(session) = sessions.login("user-1", None).block_on() else { panic!() };
let set_cookie = sessions.set_cookie(&session);
assert!(set_cookie.starts_with("__Host-id_session="));
```

Still to land in 0.9: OIDC + PKCE and TOTP.

Book: [Passwords and sessions](../id_effect/book/src/part6/ch30-03-passwords-sessions.md),
[Credentials and JWT](../id_effect/book/src/part6/ch30-04-credentials-jwt.md).

Tier: `integration` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md)).
