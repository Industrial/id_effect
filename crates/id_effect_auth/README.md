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
  cookie. Feature `axum` adds `SessionLayer`, which loads the request's session into
  `id_effect_axum`'s request capabilities.

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

Still to land in 0.9: bearer/JWT (JWKS rotation), basic and API-key credential layers producing
a principal, a database-backed session store, OIDC + PKCE and TOTP.

Book: [Passwords and sessions](../id_effect/book/src/part6/ch30-03-passwords-sessions.md).

Tier: `integration` ([ADR 0010](../../docs/adrs/0010-crate-tiers.md)).
