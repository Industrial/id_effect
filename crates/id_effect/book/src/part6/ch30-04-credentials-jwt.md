# Credentials and JWT (`id_effect_auth`)

[Passwords and sessions](ch30-03-passwords-sessions.md) cover browser logins. APIs are also
called by other services and by clients that hold a token or a key. `id_effect_auth` checks four
kinds of credential and turns each into the same thing, the request's principal, which the
[authorization](ch30-02-authorization.md) guards then judge:

| Credential | Check | Feature |
|---|---|---|
| Session cookie | `Sessions` (previous chapter) | — |
| `Authorization: Bearer <JWT>` | `JwtVerifier` (static keys or a `JwksClient`) | `jwt`, `jwks` |
| `X-API-Key: <key>` | `ApiKeys` over an `ApiKeyStore` | — |
| `Authorization: Basic <user:password>` | `BasicAuth` over a `UserLookup` + `PasswordHasher` | — |
| all of the above, per request | the `Authenticate` Tower layer | `axum` |

## The principal

Every check yields an `AuthPrincipal`: an id, roles, direct permissions, claims and a tenant,
plus the `AuthScheme` that authenticated it. It implements `id_effect_authz::Principal`, so
everything in the authorization chapter (`require_role`, `Rbac`, claim policies, `same_tenant`)
works on it unchanged.

```rust
use id_effect_auth::{AuthPrincipal, AuthScheme};
use id_effect_authz::Principal;

let p = AuthPrincipal::new("alice")
  .with_role("accountant")
  .with_permission("ledger:export")
  .with_tenant("acme")
  .with_scheme(AuthScheme::Bearer);
assert!(p.has_role("accountant"));
assert_eq!(p.tenant(), Some("acme"));
```

A rejected credential is an `AuthError`. Its `http_status()` is **401** for everything the
client got wrong (malformed, invalid, expired, not yet valid, wrong claims, unknown key), **503**
when the user store or the key endpoint is down, and **500** for unusable key material.

## Bearer tokens (feature `jwt`)

`JwtVerifier` checks a token's signature and claims and maps the claims to a principal.
Signatures, `iss` and `aud` are checked by [jsonwebtoken]; `exp` and `nbf` are checked against
a `WallClock`, with 60 seconds of leeway by default, so tests can move time. `JwtIssuer` signs
tokens, for first-party access tokens and for tests:

```rust
use std::time::Duration;

use id_effect::Exit;
use id_effect_auth::{AuthPrincipal, JwtIssuer, JwtKey, JwtVerifier};
use id_effect_authz::Principal;

let secret = b"32+ random bytes from the secret store";
let issuer = JwtIssuer::hs256(secret)
  .issuer("https://ledger.example")
  .audience("ledger-api")
  .ttl(Duration::from_secs(15 * 60));
let verifier = JwtVerifier::new([JwtKey::hmac(secret)])
  .issuer("https://ledger.example")
  .audience("ledger-api")
  .require_claim("jti");

let ada = AuthPrincipal::new("ada").with_role("accountant").with_tenant("acme");
let Exit::Success(token) = issuer.issue(&ada).block_on() else { panic!() };
let Exit::Success(p) = verifier.verify(&token).block_on() else { panic!() };
assert_eq!((p.id(), p.tenant()), ("ada", Some("acme")));
assert!(p.has_role("accountant"));
```

What the verifier enforces:

- **Keys bind algorithms.** A `JwtKey` verifies only the algorithms of its own family: an HMAC
  secret HS256/384/512, an RSA key RS* and PS*, an EC key ES256/ES384, an Ed25519 key EdDSA. A
  token that asks for HS256 can therefore never be checked with an RSA public key used as the
  HMAC secret (the classic algorithm-confusion attack). `.algorithms([..])` narrows further.
- **Issuer and audience.** Once `.issuer(..)` / `.audience(..)` are set, `iss` / `aud` are
  required and must match. A verifier with neither fails every token with `AuthError::Config`
  (since 0.10.1); `.dangerous_any_issuer()` plus `.dangerous_any_audience()` turn that off
  and record the opt-outs in the posture registry.
  Always set the audience: without one, a token minted for another service of the same issuer
  is accepted.
- **Key length.** An HMAC secret shorter than 32 bytes (`MIN_HMAC_KEY_LEN`) fails with
  `AuthError::Key`, when verifying and when issuing.
- **Time.** `exp` is required (`.require_exp(false)` relaxes it); `exp` and `nbf` honour
  `.leeway(..)`.
- **Required claims.** `.require_claim("org.id")` rejects tokens without the claim.

### Mapping claims

`ClaimMapping` decides which claims make the principal. By default the id is `sub`, roles come
from `roles`, permissions from `permissions`, `scope` (space-separated, as in OAuth 2) and
`scp`, and the tenant from `tenant`. A name with dots is first looked up as a whole (namespaced
claims such as `https://example.com/roles` contain dots), then as a path into nested objects,
which is what identity providers such as Keycloak need:

```rust
use id_effect_auth::{ClaimMapping, JwtKey, JwtVerifier};

let keycloak = ClaimMapping::default()
  .roles(["realm_access.roles", "resource_access.ledger-api.roles"])
  .tenant(Some("org"));
let verifier = JwtVerifier::new([JwtKey::hmac(b"32+ random bytes from the secret store")])
  .issuer("https://id.example.com/realms/ledger")
  .claims(keycloak);
# let _ = verifier;
```

Every claim of the token stays on the principal, so `claim_equals("email_verified", true)` and
the other attribute policies read them directly.

### Asymmetric keys

A service that issues tokens other services verify signs with a private key and publishes the
public one. `JwtIssuer::rsa_pem` / `ec_pem` load PKCS#8 PEM keys; `jwk()` is the public JSON Web
Key (with `kid`, `alg` and `use: sig`) to serve from `/.well-known/jwks.json`; `JwtKey::rsa_pem`
/ `ec_pem` / `from_jwk` load verification keys:

```rust,no_run
use id_effect_auth::{Algorithm, JwtIssuer, JwtKey, JwtVerifier};

let private = std::fs::read("keys/signing-2026-07.pem").unwrap();
let issuer = JwtIssuer::ec_pem(Algorithm::ES256, &private).unwrap().kid("2026-07");
let published = issuer.jwk().unwrap(); // serve as {"keys": [published, ..]}

let public = std::fs::read("keys/signing-2026-07.pub.pem").unwrap();
let verifier = JwtVerifier::new([JwtKey::ec_pem(&public).unwrap().with_kid("2026-07")]);
# let _ = (published, verifier);
```

An HMAC issuer refuses `jwk()`: its secret is never published.

### Key rotation with JWKS (feature `jwks`)

Identity providers rotate keys by publishing the new key in their JWKS document and then signing
with it. `JwksClient` follows that:

- it fetches the document on first use and serves keys from memory;
- a token whose `kid` is not cached triggers a refetch, so a rotated key is accepted as soon as
  it is published;
- a cache older than `max_age` (15 minutes) is refetched, so retired keys disappear;
- refetches happen at most once per `min_refresh_interval` (30 seconds), and concurrent requests
  share one fetch, so tokens with made-up `kid`s cannot turn the service into a load generator
  against the provider;
- when a refetch fails the cached keys keep working; only a client without any usable key
  answers `AuthError::Unavailable` (503).

```rust
use std::time::Duration;

use id_effect_auth::{ClaimMapping, JwksClient, JwtVerifier};

let jwks = JwksClient::new("https://id.example.com/realms/ledger/protocol/openid-connect/certs")
  .min_refresh_interval(Duration::from_secs(60))
  .max_age(Duration::from_secs(600));
let verifier = JwtVerifier::jwks(jwks)
  .issuer("https://id.example.com/realms/ledger")
  .audience("ledger-api")
  .claims(ClaimMapping::default().roles(["realm_access.roles"]));
# let _ = verifier;
```

The client uses reqwest with rustls and needs a tokio runtime, which an axum server has.

## API keys

An API key is `<prefix>_<id>_<secret>`: a prefix chosen per service (secret scanners can search
for it), 16 hex digits of public id, and 256 random bits of secret. `ApiKey::generate` returns
the plaintext, shown to its owner once, and an `ApiKeyRecord` with the SHA-256 digest of the
secret, which is all the store keeps. A secret of 256 random bits needs no slow password hash.
`ApiKeys::verify` finds the record by id and compares digests in constant time:

```rust
use id_effect::Exit;
use id_effect_auth::{ApiKey, ApiKeys, AuthPrincipal, MemoryApiKeyStore};
use id_effect_authz::Principal;

let store = MemoryApiKeyStore::new();
let (key, record) = ApiKey::generate("ledger", AuthPrincipal::new("svc-reports")).unwrap();
assert!(key.expose().starts_with("ledger_"));
store.insert(record); // a database store saves record.id(), record.digest_hex(), the principal

let keys = ApiKeys::new(store.clone(), "ledger");
let Exit::Success(p) = keys.verify(key.expose()).block_on() else { panic!() };
assert_eq!(p.id(), "svc-reports");
```

Implement `ApiKeyStore` (one method, `find(id)`) over a table; `ApiKeyRecord::from_parts`
rebuilds a record from its stored columns. Revoking a key is deleting its record.

## Basic credentials

`BasicAuth` checks `Authorization: Basic` credentials against a `UserLookup` (implement
`find_user` over the user table) with a `PasswordHasher`. It verifies with `verify_or_dummy`, so
an unknown user costs the same argon2 work as a wrong password, and both are the same
`AuthError::InvalidCredentials`. When the stored hash used older parameters, the upgraded hash
is handed to `UserLookup::update_password_hash`:

```rust
use id_effect::Exit;
use id_effect_auth::{AuthPrincipal, BasicAuth, MemoryUsers, PasswordHasher, PasswordParams};

let hasher = PasswordHasher::new(PasswordParams::new(64, 1, 1).unwrap()); // low cost for docs
let users = MemoryUsers::new();
let Exit::Success(hash) = hasher.hash("pw").block_on() else { panic!() };
users.insert("ada", hash, AuthPrincipal::new("user-ada").with_role("accountant"));

let basic = BasicAuth::new(users, hasher);
assert!(matches!(basic.authenticate("ada", "pw").block_on(), Exit::Success(_)));
assert!(matches!(basic.authenticate("ada", "nope").block_on(), Exit::Failure(_)));
```

Basic credentials travel with every request, so use them only over TLS and preferably only to
obtain a token (the example below logs in with Basic and then uses the JWT).

## The `Authenticate` layer (feature `axum`)

`Authenticate` is a Tower layer. Configure the schemes a service accepts and add it with
`Router::layer`, outside the route guards:

```rust
use axum::{Router, routing::get};
use id_effect_auth::{
  ApiKeys, AuthScheme, Authenticate, BasicAuth, JwtKey, JwtVerifier, MemoryApiKeyStore,
  MemoryUsers, PasswordHasher,
};
use id_effect_authz::guard::{AuthorizeExt, Guard};
use id_effect_authz::{authenticated, require_role};

let jwt = JwtVerifier::new([JwtKey::hmac(b"32+ random bytes from the secret store")])
  .issuer("https://ledger.example")
  .audience("ledger-api");
let auth = Authenticate::bearer_jwt(jwt)
  .with_api_key(ApiKeys::new(MemoryApiKeyStore::new(), "ledger"))
  .with_basic(BasicAuth::new(MemoryUsers::new(), PasswordHasher::default()))
  .realm("ledger");

let app: Router = Router::new()
  .route("/me", get(|| async { "me" }).authorize(Guard::new("me", authenticated())))
  .route(
    "/ledger",
    get(|| async { "entries" }).authorize(Guard::new("ledger.read", require_role("accountant"))),
  )
  .layer(auth.clone());
assert_eq!(auth.schemes(), [AuthScheme::Bearer, AuthScheme::ApiKey, AuthScheme::Basic]);
```

For each request the layer tries the configured schemes in order (by default **session →
bearer → API key → Basic**; `.order([..])` changes it) and stops at the first credential the
request carries:

| The request carries | The layer |
|---|---|
| no credential of a configured scheme, or a cookie naming no live session | continues with the `Anonymous` principal; the guard answers 401 if the route needs a login |
| a valid credential | continues with its `AuthPrincipal` |
| an invalid credential | answers **401** at once: Problem Details (`application/problem+json`, with a `scheme` member) and one `WWW-Authenticate` challenge per configured scheme; the bearer challenge carries `error="invalid_token"` (RFC 6750) |
| a credential whose check could not run (store or JWKS endpoint down) | answers **503** Problem Details |

An invalid credential is never downgraded to anonymous: a client with an expired token learns
that its token expired, rather than seeing the anonymous view of a page.

The principal is installed where `id_effect_authz`'s guard looks for it: a `CurrentPrincipal`
request extension. The same value goes into the request's `RequestCaps`, so an effect handler run
with `execute_with_request_caps` reads `CurrentPrincipal` from its environment, and the
`AuthPrincipal` itself is an extension (`Extension<AuthPrincipal>` in a plain axum handler). A
principal an outer layer already installed (a `CapOverrideLayer` for a job runner) is kept when
the request carries no credential. With a session configured, a session the layer loads is also
recorded like `SessionLayer` does, and a session already loaded by an outer `SessionLayer` is
used as is.

## Tests and example

- `tests/credentials.rs` drives an axum router with all four schemes: valid tokens, keys,
  passwords and sessions reach the handler; `expired_token_is_401` checks the Problem body and
  the challenges; missing credentials reach the guard as anonymous (401) and a principal without
  the role gets 403; store outages answer 503; the scheme order decides which credential wins.
- `tests/jwks.rs` runs a mocked identity provider: `rotated_kid_is_accepted_after_refresh`,
  made-up `kid`s are rate-limited, concurrent misses share one fetch, an outage is 503 until the
  provider recovers, and cached keys survive a failed refresh.
- `tests/jwt_keys.rs` covers RS256 and ES256 with PEM keys and published JWKs, and the
  algorithm-confusion attack.
- `examples/010_login.rs` logs in with Basic credentials, receives a JWT, calls guarded routes
  with it and with a service's API key
  (`cargo run -p id_effect_auth --features axum,jwt --example 010_login`).

[jsonwebtoken]: https://docs.rs/jsonwebtoken
