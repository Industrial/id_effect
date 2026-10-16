# Single sign-on with OpenID Connect (`id_effect_auth`)

[Passwords and sessions](ch30-03-passwords-sessions.md) keep the user's credentials in the
service. With OpenID Connect the service keeps none: the browser is sent to an identity provider
(Keycloak, Entra ID, Okta, Auth0, Google, …), the user signs in there, and the provider sends the
browser back with a one-time code that the service exchanges for an **ID token** saying who
signed in. `id_effect_auth` implements the relying-party side behind the `oidc` feature:

| Piece | What it does |
|---|---|
| `OidcConfig` | issuer, client id and secret, redirect URL, scopes, `ClaimMapping` |
| `OidcClient::discover` | reads `/.well-known/openid-configuration`, points a `JwksClient` at the provider's keys |
| `OidcClient::begin` / `exchange` | authorization URL with PKCE, state and nonce; code exchange and ID-token checks |
| `OidcRoutes` | `GET /auth/oidc/login`, `GET /auth/oidc/callback`, `POST /auth/oidc/logout` on axum |

The flow is the **authorization-code flow with PKCE** (RFC 7636, method `S256`), which the OAuth
2.0 Security Best Current Practice (RFC 9700) requires for every client, confidential ones
included. The implicit flow and the password grant are not offered.

## Configuring the client

Register the service at the provider as an OpenID Connect client with the redirect URI
`https://<your host>/auth/oidc/callback`. Then describe that registration:

```rust
use std::time::Duration;

use id_effect_auth::ClaimMapping;
use id_effect_auth::oidc::OidcConfig;

let config = OidcConfig::new(
  "https://id.example.com/realms/ledger",            // the issuer, exactly as the provider names it
  "ledger-web",                                      // the client id
  "https://ledger.example.com/auth/oidc/callback",   // the registered redirect URI
)
.client_secret("from the secret store")              // confidential client (client_secret_basic)
.scopes(["profile", "email"])                        // `openid` is always requested
.claims(ClaimMapping::default().roles(["realm_access.roles"]))
.post_logout_redirect_url("https://ledger.example.com/")
.timeout(Duration::from_secs(5));
assert_eq!(config.client_id(), "ledger-web");
```

`OidcClient::discover(config)` is an effect: run it once at start-up (it needs a tokio runtime,
as every reqwest call does) and keep the client; it is cheap to clone. Discovery fails with
`OidcError::Discovery` (503) when the document cannot be fetched, when its `issuer` is not the
configured one (a provider can only speak for itself), or when it lists
`code_challenge_methods_supported` without `S256`; an invalid configured URL is
`OidcError::Config` (500).

## Mounting the routes

`OidcRoutes` runs the browser flow over the client and the application's `Sessions`. Mount its
router next to the application's and authenticate requests with the session, as for a password
login:

```rust,no_run
use axum::{Router, routing::get};
use id_effect_auth::oidc::{OidcClient, OidcRoutes};
use id_effect_auth::{Authenticate, MemorySessionStore, SessionConfig, Sessions};
use id_effect_authz::authenticated;
use id_effect_authz::guard::{AuthorizeExt, Guard};

# fn start(client: OidcClient) {
// `client`: the result of `OidcClient::discover(config)`, run once at start-up.
let sessions = Sessions::new(MemorySessionStore::new(), SessionConfig::default());

let app: Router = Router::new()
  .route("/ledger", get(|| async { "entries" }).authorize(Guard::new("ledger", authenticated())))
  .merge(OidcRoutes::new(client, sessions.clone()).after_login("/ledger").router())
  .layer(Authenticate::session(sessions));
# let _ = app;
# }
```

What each route does:

1. **`GET /auth/oidc/login`** (optionally `?return_to=/some/path`) creates a *flow session*
   holding a random `state`, a random `nonce` and the PKCE code verifier, sets its cookie and
   answers **303** to the provider's authorization endpoint with `response_type=code`, the
   scopes, `state`, `nonce`, `code_challenge` and `code_challenge_method=S256`.
2. **`GET /auth/oidc/callback`** consumes the flow session (a callback URL works once), checks
   that `state` matches in constant time and that `iss`, when the provider sends it (RFC 9207),
   is the configured issuer, then exchanges the code together with the code verifier. The ID
   token is verified (below); its principal is logged in with `Sessions::login`, which mints a
   fresh session id and drops any id the browser presented. The principal's roles, permissions
   and tenant go into the session's attributes (`AuthPrincipal::from_session` reads them back on
   every request) together with the ID token. **303** to `return_to` or `after_login`.
3. **`POST /auth/oidc/logout`** ends the session and clears its cookie. For a session that came
   from an OIDC login, and when the provider publishes an `end_session_endpoint`, it continues to
   the provider (RP-initiated logout) with `id_token_hint`, `client_id` and the configured
   `post_logout_redirect_uri`, so the single sign-on session ends too. `.provider_logout(false)`
   keeps logout local.

Paths are configurable (`login_path`, `callback_path`, `logout_path`); the callback path must be
the path of the registered redirect URI. Callback failures answer Problem Details:

| Failure | Status |
|---|---|
| no flow session in this browser, `state` or `iss` mismatch, no `code` | **400** |
| the provider redirected with `error=` (`access_denied`, …) or refused the code (`invalid_grant`) | **401** |
| the ID token was rejected (signature, issuer, audience, expiry, nonce, `azp`) | **401** |
| the token endpoint was unreachable or answered nonsense | **502** |
| the session store or the provider's key set was unavailable | **503** |

For 5xx answers the detail is generic; the `OidcError` is attached to the response as an
extension for logging middleware.

### Why a separate flow session

`state`, `nonce` and the code verifier must survive the round trip through the provider and must
be tied to the browser that started the login, or an attacker can make a victim's browser
complete the attacker's login (login CSRF) or replay a stolen code. They are therefore kept
server-side, in a session whose id travels in the cookie `__Host-id_oidc_flow`
(`SameSite=Lax`, so the browser sends it on the provider's top-level redirect back; `HttpOnly`;
10 minutes). That cookie is not the application's session cookie, so a browser that started a
login but did not finish it is still anonymous to `Authenticate`.

The flow sessions are in memory by default. With several instances behind a load balancer, give
them a shared store with the same configuration:

```rust
use id_effect_auth::oidc::{OidcRoutes, DEFAULT_FLOW_COOKIE, FLOW_TIMEOUT};
use id_effect_auth::{MemorySessionStore, Sessions};

let config = OidcRoutes::flow_session_config();
assert_eq!((config.cookie.name.as_str(), config.absolute_timeout), (DEFAULT_FLOW_COOKIE, FLOW_TIMEOUT));
// In production: a `PgSessionStore` shared by every instance.
let flow = Sessions::new(MemorySessionStore::new(), config);
# let _ = flow;
```

and pass it with `OidcRoutes::flow_sessions(flow)`.

## ID-token checks

`OidcClient::exchange` (and the callback) verify the ID token with the crate's `JwtVerifier`
over a `JwksClient` for the provider's `jwks_uri`, so everything from
[Credentials and JWT](ch30-04-credentials-jwt.md) applies: keys bind algorithms, a token signed
with a newly rotated key triggers one rate-limited JWKS refetch, and time runs on a `WallClock`
(`OidcConfig::with_clock`) with 60 s of leeway. On top of the JWT checks:

- `iss` must be the discovered issuer and `aud` must contain the client id;
- `sub` and `iat` must be present and `exp` must not have passed;
- `nonce` must equal the one stored for this login (constant-time comparison), so a token
  minted for another login cannot be replayed;
- `azp`, when present, must be the client id, and it is required when `aud` has several values.

The claims then go through the `ClaimMapping`: `sub` is the principal id by default, roles come
from `roles` (Keycloak puts realm roles in `realm_access.roles`), and every claim (`email`,
`name`, …) stays on the principal for attribute policies. The principal's scheme is
`AuthScheme::Session`, since that is how later requests authenticate.

## Without the routes

`OidcClient` can drive other front ends (a single-page app's backend-for-frontend, a CLI
helper). Keep `AuthorizationRequest::pending` server-side between the two calls:

```rust,no_run
use id_effect_auth::oidc::{OidcClient, PendingLogin};

# fn flow(client: OidcClient, state_from_callback: &str, code: &str) {
let request = client.begin();
// send the browser to `request.url`; store `request.pending` (e.g. `pending.store(&mut session)`)
let pending: PendingLogin = request.pending;
assert!(pending.state_matches(state_from_callback));
let login = client.exchange(code, &pending); // Effect<OidcLogin, OidcError>
# let _ = login;
# }
```

`OidcLogin` carries the principal, the raw ID token and the provider's access and refresh tokens.
`OidcClient` also implements `id_effect_platform::auth::OAuthClient`, for code written against
that capability. The trait has no place for a code verifier or a nonce, so its authorization URL
carries neither; a provider that enforces PKCE (as the Keycloak test realm does) refuses it.
Prefer `begin` / `exchange` for browser logins.

## Tests

- `tests/oidc.rs` mocks a provider with wiremock (discovery, JWKS, token endpoint; ID tokens
  signed with a generated EC key): the full login checks the authorization URL, that the code
  verifier sent to the token endpoint hashes to the challenge, the session cookie, `/me` as the
  logged-in user, that the callback works once, and RP-initiated logout. Other tests cover a
  forged or missing `state`, a provider error, an `iss` mix-up, a foreign `nonce`, a wrong
  audience, an expired token, an unknown signing key, `invalid_grant`, discovery naming another
  issuer or lacking `S256`, store outages, and the `OAuthClient` view.
- `tests/oidc_keycloak.rs` starts Keycloak 26 in a container (testcontainers) with the realm in
  `tests/fixtures/keycloak/` (client `ledger-web` requiring PKCE S256, user `alice` with realm
  role `accountant`), runs the application on a socket, and plays the browser: it follows the
  redirects, posts the credentials to Keycloak's login form, and lands on `/me` logged in as
  `alice` with the `accountant` role; logout ends the Keycloak session too. Without Docker it
  prints `SKIP`; CI's `containers-integration` job sets `ID_EFFECT_REQUIRE_DOCKER=1`, which turns
  a missing Docker into a failure.

```text
ID_EFFECT_REQUIRE_DOCKER=1 cargo nextest run -p id_effect_auth --features oidc
```
