# Migrating to id_effect 0.11

This guide is the "if you used X, now do Y" companion to the `0.11.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.10. If you are on 0.9, apply
[MIGRATION-0.10.md](MIGRATION-0.10.md) first.

Every intentional public-API break in 0.11 is listed here. A break reported by
`cargo semver-checks` (the `semver` CI job, baseline the last 0.10.x release) that is not listed
below is a bug; the last section maps each reported break to its entry. The deprecation rule and
semver policy are in [STABILITY.md](STABILITY.md).

0.11 is the secure-by-default release (ADR 0013): what was an opt-in is now the default, and the
way back is a named `dangerous_*` / `insecure_*` call that records itself in the posture
registry. Where the plan keeps the 0.10 spelling for one minor (rule L9), the entry names the
deprecated shim, removed in 0.12; where there is none, it says why.

**Mesh peers: 0.11 is the minimum.** The mesh protocol is version 3; a 0.11 node refuses a
0.10.x peer in the handshake. Upgrade every node of a cluster together ("Node: mesh protocol
version").

## Upgrade checklist

In the order you are most likely to hit them:

1. **Typed routes declare who may call them.** Add `.public()` or `.guard(g)` to every `Route`
   (or endpoint); without one `Router::route` does not compile. Shim: `Router::route_unchecked`
   (deprecated, recorded). See "Routing".
2. **`Secret<T>` needs `T: Zeroize`**, and `==` exists only on `SecretString` / `SecretBytes`.
   No shim. See "Config: `Secret<T: Zeroize>`".
3. **SQL built at run time needs a capability.** `query(format!(..), ..)` no longer compiles:
   bind the value, or run a `RawSql` with `query_raw` and `RawSqlCapability::layer_dangerous()`.
   No shim. See "SQL".
4. **The mesh is mutual TLS by default; plaintext TCP is behind the `insecure-plaintext-tcp`
   feature.** A QUIC listener needs client trust; `TcpTransport` becomes
   `TcpTransport::try_tls(config)` (feature `tls-tcp`) or `TcpTransport::dangerous_plaintext()`.
   See "Node: QUIC requires client certificates" and "Node: plaintext TCP".
5. **Mesh protocol v3.** No mixed 0.10/0.11 clusters. See "Node: mesh protocol version".
6. **The HTTP client refuses private destinations and follows no redirect.** Allowlist internal
   hosts (`allow_hosts`, `allow_cidrs`) and opt into redirects (`follow_redirects(max)`). See
   "HTTP client".
7. **The prod posture check.** Generated 0.11 projects run it; your app opts in with
   `Posture::load(..)….check().enforce()`. In `prod` it refuses every recorded opt-out (each
   `dangerous_*` / `insecure_*` call, `MemoryTotpStepStore`, `MemoryThrottleStore`) not listed
   under `[security.accepted_risks]` with a reason. See "Kit and templates" and "Auth: shared
   security state".
8. **New struct fields.** `HostConfig` gains `load_shed`, `HttpClientConfig` gains `ssrf` and
   `max_redirects`: add `..HostConfig::default()` / `..HttpClientConfig::default()` to struct
   literals. See "Web: load shedding" and "HTTP client".
9. **Deprecated names → `dangerous_*` / `insecure_*`.** The 0.10 spellings below still compile
   (with a warning) and record the same posture id; rename them before 0.12.
10. **Smaller breaks:** `#[non_exhaustive]` enums in `id_effect_process` (and new variants in
    `id_effect_node`) need wildcard arms; logs are redacted by default; the CSRF token is masked;
    tenanted resources check the tenant; Redis `clear()` refuses the empty namespace; STM moves
    to `id_effect::stm`; `TcpTransport` and `ConsoleConfig` are no longer `Copy`; `OpenApiUi` is
    no longer `UnwindSafe`.

### Deprecated in 0.11 (one-minor shims, removed in 0.12)

| 0.10 | 0.11 | Posture id the call records |
|---|---|---|
| `Router::route(endpoint)` without a decision | `.public()` / `.guard(g)`, or `Router::route_unchecked` (deprecated) | `id_effect_axum::route::Router::route_unchecked` |
| `RedactionPolicy::new()` | `RedactionPolicy::recommended()`, or `dangerous_empty()` | `id_effect_logger::RedactionPolicy::dangerous_empty` |
| `SecurityLayer::without_csrf()` | `SecurityLayer::dangerous_without_csrf()` | `id_effect_axum::server::security::SecurityLayer::dangerous_without_csrf` |
| `BasicAuth::without_throttle_dangerous()` | `BasicAuth::dangerous_without_throttle()` | `id_effect_auth::basic::BasicAuth::dangerous_without_throttle` |
| `Totp::without_lockout_dangerous()` | `Totp::dangerous_without_lockout()` | `id_effect_auth::totp::Totp::dangerous_without_lockout` |
| `JwtVerifier::any_issuer_dangerous()` / `any_audience_dangerous()` | `dangerous_any_issuer()` / `dangerous_any_audience()` | `id_effect_auth::jwt::JwtVerifier::dangerous_any_issuer` / `…::dangerous_any_audience` |
| `JwksClient::allow_http_dangerous()` | `JwksClient::insecure_allow_http()` | `id_effect_auth::jwks::JwksClient::insecure_allow_http` |
| `provide_reqwest_client(client)` | `dangerous_provide_reqwest_client(client)`, or `ReqwestClientLive::layer_with(config)` | `id_effect_platform::http::reqwest::dangerous_provide_reqwest_client` |
| `ReqwestHttpClient::new(client)` | `ReqwestHttpClient::dangerous_unguarded(client)`, or `from_config(&config)` | `id_effect_platform::http::ReqwestHttpClient::dangerous_unguarded` |
| `id_effect_fp::stm::*`, `id_effect_fp::{Stm, TRef, …}` | `id_effect::stm::*` | none |

## Web (`id_effect_axum`, `id_effect_console`)

### Routing: every route is public or guarded (H8)

Deny by default: every typed route declares who may call it (ADR 0013 R1; finding H8, decision
D8). Book chapter "Typed routes and validation", section "Every route declares its access".

- **`route::Router::route` only accepts a decided endpoint.** `Route`, `Endpoint` gain an access
  type parameter (`Route<P, Q, H, B, A = Undecided>`, `Endpoint<S, A = Undecided>`); call
  `.public()` (anyone, anonymous callers included) or `.guard(g)` (any `RouteGuard`) on the
  route, or `.public()` / `.guard(g)` / `.authorize(guard)` on the endpoint. Without one the call
  fails with `the route has no access decision`:

  ```rust,ignore
  // 0.10
  Router::new().route(Route::get("/accounts/{id}").path::<AccountId>().handler(show))
  // 0.11
  Router::new().route(Route::get("/accounts/{id}").public().path::<AccountId>().handler(show))
  Router::new().route(Route::get("/accounts/{id}").guard(Guard::new("accounts.read", policy)).path::<AccountId>().handler(show))
  ```

  Code that names the types keeps compiling where the defaults fit; a function returning a
  guarded endpoint now returns `Endpoint<S, Guarded>` (`AuthorizedEndpoint::into_endpoint` and
  `Endpoint::from(authorized)` do).
- **`RouteGuard`** (`id_effect_axum::route`) is the guard trait: `policy()` names it in
  `RouteInfo::policies` and OpenAPI, `check(&Parts)` lets the request through or answers it.
  `id_effect_authz::Guard` implements it.
- **On a typed route an `id_effect_authz::Guard` answers 401 to the anonymous principal before
  its policy runs**, even a policy that admits everyone (`allow_all()`, a registry policy named
  `"public"`). A typed route anonymous callers use (sign-up, login) is `.public()`. As a plain
  Tower layer (`MethodRouter::authorize`, `.layer(guard)`) the guard is unchanged.
- **`RouteInfo` has a new field `access: RouteAccess`** (`Undecided`, `Public`, `Guarded`,
  `Unchecked`; `#[non_exhaustive]`) and compares it in `==`. `RouteInfo::new` is `Undecided`;
  `Endpoint::from_parts` always yields an undecided endpoint, so decide again after
  reassembling one. The `/_routes` table (`RouteEntry`) gains `access`.
- **`into_axum()` and `into_parts()` install a deny-by-default layer**: a request whose matched
  route is not a registered public, guarded or unchecked typed route answers **404** Problem
  Details. That affects plain axum routes merged into the typed router with the new
  `Router::merge_axum`; turn a plain handler into a decided endpoint
  (`Endpoint::from_parts(RouteInfo::new(method, path), method_router).public()`) or opt out with
  `Router::allow_unregistered_routes_dangerous()`, which records the posture opt-out
  `id_effect_axum::route::Router::allow_unregistered_routes_dangerous`. Routes merged onto the
  returned `axum::Router` are outside the layer; add `Router::access_layer()` last
  (`.route_layer(access)`) to cover them.
- **`Router::access_report()`** lists the public routes, the guarded routes with their policies
  and any opt-out; `into_axum()` logs it at startup (`tracing`, target
  `id_effect_axum::route::access`; a warning when an opt-out is present).
- **OpenAPI**: a public operation has `security: []` (no authentication); guarded operations are
  unchanged.
- **Shim (removed in 0.12):** `Router::route_unchecked(endpoint)` is `#[deprecated]` and routes an
  undecided endpoint the 0.10 way (served to everyone). It records it as `RouteAccess::Unchecked`
  and the posture opt-out `id_effect_axum::route::Router::route_unchecked`, which the prod
  posture check rejects unless accepted.

### Web: CSRF masked token format

`SecurityLayer`'s CSRF token is masked per response against BREACH (ADR 0013 R9; findings L1,
L5, M4) and the default headers gain cross-origin isolation (L2). No shim for the token format:
an unmasked token is what BREACH reads. `without_csrf()` keeps a deprecated shim (below).

- **`CsrfToken::as_str()` is masked.** It is base64url of a fresh 32-byte pad followed by the pad
  XOR the session secret: 86 characters instead of 43, and different on every response. Every
  masked token of a session validates while its cookie holds the same secret, so render
  `as_str()` into each page as before. Code that cached the token string, compared two
  `as_str()` values, or checked its length must stop doing so; `CsrfToken`'s `==` compares the
  secrets. The cookie format is unchanged, so sessions survive the upgrade, but a page rendered
  by 0.10 (an unmasked 43-character token) is rejected with 403 after the deploy: reload it.
- **`CsrfToken::rotate()`** issues a new secret, returns its token and makes the response set
  the new cookie; tokens rendered before stop validating for that client. Call it at login
  (`id_effect_auth` does).
- **`CsrfConfig` is no longer deprecated:** `CsrfConfig::recommended()` (also `Default`) holds the
  form-body limit (`form_limit`, default 64 KiB, was 2 MiB) and read timeout (`read_timeout`,
  default 10 s), installed with `SecurityLayer::csrf_config`. A urlencoded body over the limit
  is rejected with 413, a slower one with 408, whatever layers wrap `SecurityLayer`. Only the
  0.7 constructor `CsrfConfig::new(token)` stays deprecated; the deprecated `csrf_middleware`
  now refuses every unsafe request when the config's token is empty.
- **`SecurityHeaders::recommended()`** adds `Cross-Origin-Opener-Policy: same-origin`,
  `Cross-Origin-Resource-Policy: same-origin` and `Permissions-Policy: camera=(),
  microphone=(), geolocation=(), payment=(), usb=()`. Pages that open OAuth popups set
  `.cross_origin_opener_policy("same-origin-allow-popups")?`; assets embedded by other origins
  set `.cross_origin_resource_policy("cross-origin")?` (or `same-site`); each has a
  `without_*` switch, and a header the handler sets itself still wins.
- **`SecurityLayer::without_csrf()` is deprecated** (removed in 0.12) for
  `SecurityLayer::dangerous_without_csrf()`. Both switch CSRF off and record the posture opt-out
  `server::security::DANGEROUS_WITHOUT_CSRF`
  (`id_effect_axum::server::security::SecurityLayer::dangerous_without_csrf`), which the prod
  posture check rejects unless accepted. An app that also serves bearer tokens can keep CSRF for
  cookie requests with `csrf_exempt_bearer()` instead. `SecurityLayer::csrf_enabled()` says
  whether the layer checks tokens; `id_effect_kit` `Posture::security_layer(&layer)` states the
  "CSRF on" probe from it in place of `.csrf(bool)`.

### Web: load shedding and `HostConfig::load_shed`

Every server bounds its in-flight requests (ADR 0013 R4; findings M5, M8). No shim: a new public
field cannot be deprecated in; `..HostConfig::default()` is the one-line fix.

- **`HostConfig` has a new public field `load_shed: LoadShed`** (`id_effect_axum::middleware`),
  and `HostConfig` implements `Default` (`0.0.0.0:8080`, 30 s, the default `LoadShed`). A struct
  literal without the field fails with `missing field load_shed`: add `..HostConfig::default()`
  or `load_shed: LoadShed::default()`. `HostConfig::config()` loads it from the new optional
  `MAX_IN_FLIGHT` (at least 1); `HostConfig::descriptor()` now yields a 4-tuple
  `(String, i64, i64, Option<i64>)`.
- **The server entry points answer `503` beyond the cap.** `server_layer*`, `serve*`,
  `serve_router*` and `ServerHandle::start*` wrap the router in `LoadShedLayer`: past
  `LoadShed::default_max_in_flight()` (512 per available CPU) a request gets `503` Problem
  Details with `Retry-After: 1` at once, never a queue. Raise it with `LoadShed::new(n)` (or
  `MAX_IN_FLIGHT`) for a server that legitimately holds many long requests; turn it off only
  with `HostConfig::dangerous_no_load_shedding()`, which records the posture opt-out
  `id_effect_axum::HostConfig::dangerous_no_load_shedding`. A router served through
  `serve_listener` or `axum::serve` directly adds `LoadShedLayer` itself.
- `MemoryRateLimitStore` is sharded (`shards(n)`, default four per CPU, at most 64) and evicts
  per shard in least-recently-used order, so with more than one shard the bucket dropped is the
  oldest of its shard, not of the whole store. `shards(1)` keeps the exact global order. Its
  buckets are no longer `RateLimiter`s: `RateLimitLayer` checks them inline on the wall clock
  (the new provided `RateLimitStore::try_acquire_now`), and `acquire` refills on the executor
  clock as before. A custom `RateLimitStore` compiles unchanged.

### Web: `/docs`, `/metrics` and the console are closed in prod

Operator surfaces need a guard outside development (ADR 0013 R7; findings M6, M7, L2). No shim;
the explicit opt-out is `OpenApiUi::public_dangerous()`.

- **`OpenApiUi::router` (and `openapi_router`, `OpenApi::routes`) mounts nothing outside the
  `dev` and `test` profiles** unless the UI is guarded or explicitly public. The profile is
  `OpenApiUi::profile(..)`, else the `ID_EFFECT_PROFILE` variable, else `dev`; an app that
  selects its profile with `--profile` passes it: `OpenApiUi::scalar().profile(profile.as_str())`.
  Keep the docs in prod with `.guard(g)` (any `RouteGuard`, such as an `id_effect_authz`
  `Guard`; applies in every profile) or `.public_dangerous()`, which records the posture opt-out
  `id_effect_axum::openapi::OpenApiUi::public_dangerous`. `OpenApiUi::is_mounted()` tells which.
- **The Scalar bundle is pinned**: `DEFAULT_SCALAR_URL` is
  `@scalar/api-reference@1.75.0/dist/browser/standalone.js` with the Subresource Integrity hash
  `DEFAULT_SCALAR_INTEGRITY`, set by `OpenApiUi::scalar()`. `cdn_url(..)` with another URL drops
  that default hash: pass the mirrored file's own with `.integrity(..)`. `OpenApiUi` no longer
  derives `PartialEq`; it implements it (guards compare by identity). It holds its guards as
  `Arc<dyn RouteGuard>`, so it is no longer `UnwindSafe` / `RefUnwindSafe`: code that moves one
  into `std::panic::catch_unwind` wraps it in `AssertUnwindSafe`. No shim (the guards are the
  change).
- **`metrics::guarded_metrics_router(guard)`** serves `/metrics` behind a `RouteGuard`. Keep
  `metrics_router()` for an operator listener only.
- **`ConsoleConfig` is `Clone`, no longer `Copy`**, and carries the hosts the console answers to
  (`allow_hosts(..)`, `allowed_hosts()`, `allows_host(..)`; default: the bind address, plus
  `localhost` on loopback). Wrap the console's routes on its own listener in
  `config.check_host_and_origin(routes)`: a request for another `Host`, or with an `Origin`
  outside the list (`null` included), gets `403` Problem Details before the guard runs. Code that
  copied a `ConsoleConfig` clones it.
- **Generated services** (`id-effect new --template service`): `/metrics` needs
  `Authorization: Bearer $METRICS_TOKEN` (unset: every scrape is `401`); `BodyLimitLayer` and
  `TimeoutLayer` wrap `SecurityLayer`; with `--console`, the local-operator shortcut exists only
  when `ID_EFFECT_PROFILE` is unset or `dev` (`config/deploy.yml` sets `prod`), the console checks
  Host and Origin against `CONSOLE_ALLOWED_HOSTS` (default: its bind address), and
  `console::routes(.., dev)` builds it for either profile. Port an existing service by hand.
- **The Ledger** enables `[console]` in `config/dev.toml` (and the tests' `test.toml`) only;
  `base.toml` sets `enabled = false`, so prod has no console.

## Data (`id_effect_sql`, `id_effect_redis`)

### SQL: `Sql` and `RawSql`

Raw SQL needs a capability (ADR 0013 R5; finding M16, decision D9). No shim: a deprecated
`From<String>` would keep the injection path open for every caller; the escape hatch is
`query_raw` with `RawSqlCapability`.

- **`query`, `execute`, `query_as`, `query_one_as`, `query_opt_as`, `query_stream` and
  `query_stream_as` take `impl Into<Sql>` instead of `impl Into<String>`.** Only a
  `&'static str` converts, so a literal or a `const` compiles unchanged (`const BY_ID: Sql =
  Sql::new("…")` works too) and a `String` does not: `query(format!("… {x}"), vec![])` is now
  `the trait From<String> is not implemented for Sql`. Move the value into a bound parameter
  (`query("… WHERE id = $1", params![x])`). The statement helpers of the `query` feature
  (`execute_query`, `fetch_*_as`) and `Repository` are unchanged.
- **SQL that is really built at run time** (a table name from an allowlist, generated DDL) is a
  `RawSql`, run with `query_raw(RawSql::new(sql), params)` / `execute_raw(..)`. Both need a
  `RawSqlCapability` in `R` besides the client; it has no public constructor, and
  `RawSqlCapability::layer_dangerous()` provides it (merge it with the client layer:
  `sql_client_layer(c).merge(RawSqlCapability::layer_dangerous())`, environment
  `HCons<SqlClientService, RawSqlEnv>`). Taking it records the posture opt-out
  `id_effect_sql::RawSqlCapability::layer_dangerous`, which the prod posture check reports.
- The `SqlClient` and `SqlTransaction` trait methods still take `&str` (the driver boundary,
  revisited in 0.18); the `raw_sql_non_literal` lint warns on a non-literal argument there.
- `id_effect_test`: `Fixture::insert`'s default now runs `insert_sql` through
  `SqlClient::query` (it returns a `String`); behaviour is unchanged.

### Redis: `clear()` on an empty namespace

Destructive operations cannot target everything by accident (ADR 0013 R5; finding M17). No shim;
the replacement is `clear_all_dangerous()`.

- **`RedisKv::clear`, `RedisHash::clear` and `RedisJson::clear` on a store whose namespace is
  `""` fail with the new `RedisError::EmptyNamespace`** instead of deleting every key of the
  database (`RedisError` is `#[non_exhaustive]`; a `match` needs a wildcard already). A namespaced
  store clears only its own `namespace:*` keys, as before. `keys()` / `ids()` are unchanged.
- To flush on purpose, call `clear_all_dangerous()` on the store: it deletes what `clear()` would
  and, for the empty namespace, every key; it records the posture opt-out
  `id_effect_redis::<Store>::clear_all_dangerous`.
- `RedisCacheBackend` on the empty namespace: `invalidate_all` now fails (counted as a backend
  error) instead of flushing the database; give the cache a namespace.

## Secrets, logs, auth and authorization

### Config: `Secret<T: Zeroize>` and its equality

Secrets are zeroized on drop and compared in constant time (ADR 0013 R2; findings M13, L3). No
shim: a trait bound and a removed `PartialEq` impl cannot be deprecated.

- **`Secret<T>` requires `T: zeroize::Zeroize`** and overwrites its value when dropped. `String`,
  `Vec<u8>`, the integers and byte arrays implement it; `Secret<String>`, `Secret<Vec<u8>>` and
  every value loaded by `Config::…().secret()` or `VaultProvider::secret` compile unchanged. A
  `Secret<&str>` or a `Secret` of your own type now fails with `the trait bound T: Zeroize is not
  satisfied`: hold an owned `String`, or implement `Zeroize` (or `#[derive(Zeroize)]` with
  zeroize's `derive` feature). Generic code over `Secret<T>` adds the `T: Zeroize` bound;
  `Config::<T>::secret` and `VaultProvider::secret::<T>` carry it too. `into_inner()` hands the
  value to the caller, who then owns wiping it.
- **New aliases** `SecretString` (`Secret<String>`) and `SecretBytes` (`Secret<Vec<u8>>`).
- **Equality exists only on `SecretString` and `SecretBytes`** and uses
  `subtle::ConstantTimeEq` (a length difference still returns early). The generic
  `impl<T: PartialEq> PartialEq for Secret<T>` is removed, so `==` on, say, `Secret<u64>` no
  longer compiles: compare `a.expose() == b.expose()` where timing does not matter, or keep the
  value as a string or bytes.
- `id_effect_auth::Password` zeroizes its plaintext on drop (it implements `Zeroize` and
  `ZeroizeOnDrop`); its API is unchanged.
- `id_effect_auth::Session`'s `Debug` no longer prints attribute values (they can hold OIDC
  tokens): it shows the id prefix, the user id, the attribute keys (`attr_keys`) and the
  timestamps. Read values with `attr(key)` / `attrs()`.

### Logging: `RedactionPolicy::new`

Logs are redacted and escaped by default (ADR 0013 R3; findings M11, M12).

- **No policy installed now means `RedactionPolicy::recommended()`**, not "redact nothing". It
  masks the annotation keys `password`, `passwd`, `secret`, `token`, `access_token`,
  `refresh_token`, `id_token`, `api_key`, `apikey`, `authorization`, `cookie`, `set-cookie`,
  `session`, `csrf`, `private_key`, `client_secret` and the globs `*_token`, `*_secret`,
  `*password*` (ASCII case-insensitive). `RedactionPolicy::default()` is the same policy.
- **`RedactionPolicy::new()` is deprecated** (L9 shim, removed in 0.12). It still returns a
  policy with no rules, by calling `dangerous_empty()`, so it records the same opt-out. Replace
  it with:
  - `RedactionPolicy::recommended().field(..)…` to keep the default rules and add yours (an
    installed policy replaces the default, it does not extend it);
  - `RedactionPolicy::dangerous_empty().field(..)…` to pick every rule yourself (the exact old
    behaviour). It is recorded as the posture opt-out `id_effect_logger::DANGEROUS_EMPTY`
    (`id_effect_logger::RedactionPolicy::dangerous_empty`), which the prod posture check
    rejects unless accepted;
  - `RedactionPolicy::dangerous_none()` to log in clear on purpose. It is recorded as the posture
    opt-out `id_effect_logger::RedactionPolicy::dangerous_none`, which the prod posture check
    rejects unless accepted.

  Prefer `RedactionPolicy::recommended()` plus your rules, which records nothing. (An `empty()`
  constructor existed briefly during 0.11 development and was removed before release.)
- `active_redaction_policy()` returns the policy the logger applies (installed or default), and
  `RedactionPolicy::is_empty()` says whether it has any rule.
- **Line formats escape untrusted text.** `TracingLogBackend` writes CR, LF, tab as `\r`, `\n`,
  `\t` and every other control character (ANSI `ESC`, C1, DEL, U+2028/U+2029) as `\u{..}` in the
  message, target, spans, keys and values; spaces, `=`, `"` and `\` in keys become `\u{20}`,
  `\u{3d}`, `\u{22}`, `\u{5c}`; a value that is empty or holds a space, `=` or `"` is written in
  double quotes with `"` and `\` escaped. `StructuredLogBackend` escapes keys the same way (its
  values were already quoted). A log parser that split on raw spaces in keys, or matched raw
  newlines or colour codes in messages, needs updating; the JSON backend is unchanged.

### Auth: shared security state and rotation at login

Security state that must be shared has a persistent store (ADR 0013 R10; finding M18, the H5
follow-up), and logging in rotates the CSRF token (R9; finding L5). Nothing breaks at compile
time; a service with more than one instance should switch stores, and the prod posture check now
reports the in-memory ones.

- **Apply migration `0003_id_effect_auth_state`** (`crates/id_effect_auth/migrations/`) with the
  crate's other migrations, or call `PgTotpStepStore::ensure_schema()` /
  `PgThrottleStore::ensure_schema()` at startup (either creates both tables,
  `id_effect_totp_steps` and `id_effect_auth_failures`).
- **TOTP replay protection across instances:** replace
  `Totp::new(config, MemoryTotpStepStore::new())` with
  `Totp::new(config, PgTotpStepStore::new(client))` (features `pg` + `totp`). Creating a
  `MemoryTotpStepStore` now records `id_effect_auth::MemoryTotpStepStore::memory_only` in the
  posture registry, which the prod posture check reports as memory-only security state; tests
  and single-process development keep it.
- **The default login throttle and TOTP lockout are recorded as memory-only.**
  `MemoryThrottleStore` (the store of `LoginThrottle::default()`, and so of `BasicAuth::new` and
  of `Totp::new`'s lockout) counts failures per process, so N instances allow N budgets of
  guesses. Creating one records `id_effect_auth::MEMORY_THROTTLE_STORE_POSTURE`
  (`id_effect_auth::MemoryThrottleStore::memory_only`), and the kit probe "no memory-only
  security state" fails on it in `prod`. Pick one:
  - **Share the counts** (feature `pg`): apply migration `0003_id_effect_auth_state` (or call
    `PgThrottleStore::ensure_schema()`), then build with the constructors that never create the
    memory store:

    ```rust,ignore
    // recorded: `new` creates the memory store before it is replaced
    let basic = BasicAuth::new(users, hasher).with_throttle(LoginThrottle::new(PgThrottleStore::new(client.clone())));
    let totp = Totp::new(config, PgTotpStepStore::new(client.clone()))
      .with_failure_store(PgThrottleStore::new(client));
    // not recorded
    let basic = BasicAuth::throttled(users, hasher, LoginThrottle::new(PgThrottleStore::new(client.clone())));
    let totp = Totp::with_stores(config, PgTotpStepStore::new(client.clone()), PgThrottleStore::new(client));
    ```

    `.with_throttle(..)` / `.with_failure_store(..)` after `new` still record the opt-out: the
    registry only adds, so the memory store `new` created stays recorded. Schedule
    `PgThrottleStore::purge_forgotten(now - forget_after)` so keys of made-up account names do
    not accumulate.
  - **Accept the risk** for a single-instance deployment: list
    `"id_effect_auth::MemoryThrottleStore::memory_only"` under `[security.accepted_risks]` with a
    reason ("Kit and templates: the prod posture check").

  No shim: nothing is renamed; the change is that the posture registry now sees the store. The
  Ledger counts both in one `PgThrottleStore` (its migration `0009_auth_failures`).
- **Rotate the CSRF token at login:** behind `SecurityLayer`, call
  `sessions.login_with_csrf(user, presented, &csrf_token)` (feature `axum`) instead of
  `sessions.login(user, presented)` and return or render the token it hands back; the layer sets
  the new CSRF cookie, so tokens rendered before login stop validating. `login` itself is
  unchanged (it already issues a fresh session id and removes the presented one).
- **The 0.10.1 auth opt-outs are renamed to the ADR 0013 form and recorded** (L9 shims, removed
  in 0.12): `BasicAuth::without_throttle_dangerous()` is now `dangerous_without_throttle()`,
  `Totp::without_lockout_dangerous()` is `dangerous_without_lockout()`, and
  `JwtVerifier::any_issuer_dangerous()` / `any_audience_dangerous()` are
  `dangerous_any_issuer()` / `dangerous_any_audience()`. Old and new names alike record
  `id_effect_auth::basic::BasicAuth::dangerous_without_throttle`,
  `id_effect_auth::totp::Totp::dangerous_without_lockout`,
  `id_effect_auth::jwt::JwtVerifier::dangerous_any_issuer` and
  `id_effect_auth::jwt::JwtVerifier::dangerous_any_audience` (`basic::DANGEROUS_WITHOUT_THROTTLE`,
  `totp::DANGEROUS_WITHOUT_LOCKOUT`, `jwt::DANGEROUS_ANY_ISSUER`, `jwt::DANGEROUS_ANY_AUDIENCE`)
  in the posture registry, so the prod posture check refuses them unless the risk is accepted.

### Authorization: tenant match by default (M19)

Resources scoped to a tenant require the tenant match (ADR 0013 R1; finding M19). No shim; the
explicit opt-out is `dangerous_cross_tenant(reason)`.

- **Every policy defined for a `Tenanted` resource checks the tenant first.**
  `PoliciesBuilder::define::<Res>`, `check_with` and `authorize_with` put the policy behind the
  new `TenantScoped` check: on a resource whose `TenantScope` is a tenant, a principal of another
  tenant, or of none, is denied with `resource belongs to another tenant` before the policy runs.
  A definition that relied on reaching another tenant's resource (an ACL entry or a role granted
  to a principal outside the tenant) is now denied. Existing `.and(same_tenant())` stays correct
  and is now redundant; `same_tenant()` remains for policies evaluated directly
  (`Policy::evaluate` is the bare rule; wrap it in `tenant_scoped(..)` for the check).
- **Resource types passed to `define`, `check_with` and `authorize_with` implement the new
  `TenantScope` trait.** `Tenanted` types do so already (blanket impl); `()`, `str`, `String` and
  `Acl` have no tenant. For your own resource of no tenant, add `impl TenantScope for Report {}`
  (the error without it: `the trait bound Report: TenantScope is not satisfied`). A type cannot
  implement both `Tenanted` and `TenantScope` by hand.
- **Crossing tenants on purpose** (support tooling, a platform operator):
  `policy.dangerous_cross_tenant("reason")` returns a `CrossTenant` policy that skips the check.
  Make it the outermost call (`a.and(b).dangerous_cross_tenant(..)`, not
  `a.dangerous_cross_tenant(..).and(b)`). It records the posture opt-out
  `id_effect_authz::PolicyExt::dangerous_cross_tenant`, which the prod posture check rejects
  unless accepted.
- `Policy` has a new provided method `crosses_tenants()` (default `false`); implementations need
  no change.

### Auth and authz: optional synchronous fast paths

`id_effect_auth::ApiKeyStore` gains `find_now` and `id_effect_authz::Policy` gains `decide_now`,
both with a default body returning `None`. Nothing changes for a custom store or policy that
ignores them: `Authenticate` runs `find`, and a route guard runs `evaluate`, as in 0.10.

- A store that looks keys up without I/O (an in-process map) can implement
  `find_now(&self, id) -> Option<Result<Option<ApiKeyRecord>, AuthError>>` and return
  `Some(outcome)`; `Authenticate` then checks the key inline, without the effect runtime.
- A policy that decides from its arguments alone can implement
  `decide_now(&self, principal, resource) -> Option<Decision>` and return `Some` of the decision
  `evaluate` would yield; a guard without audit events then decides without a fiber.

Return `None` whenever the answer needs I/O, and keep the two paths in agreement: a fast path
that decides differently from `find`/`evaluate` is a bug.

## Outbound HTTP (`id_effect_platform`, `id_effect_auth` OIDC and JWKS)

### HTTP client: SSRF guard on by default

Outbound requests are guarded (ADR 0013 R6; finding M3, decision D10). Every client
`id_effect_platform` builds — `ReqwestHttpClientProvider::layer()` / `layer_with(config)`,
`ReqwestHttpClient::default_client()`, `with_timeouts` and the new `from_config` — now:

- **refuses private destinations:** loopback, unspecified, RFC 1918, `100.64.0.0/10`,
  link-local (`169.254.0.0/16`, `fe80::/10`, so cloud metadata `169.254.169.254` and
  `fd00:ec2::254`), unique local `fc00::/7`, multicast, reserved/broadcast, and every
  IPv4-mapped or IPv4-compatible IPv6 form. A literal-IP URL is checked before sending and again
  by the client's connector (so `http://169.254.169.254/` and `http://[::1]/` are refused on the
  bare `reqwest::Client` too); a host name is checked by the client's DNS resolver on every
  address it resolves to (one forbidden address refuses the name), so DNS rebinding cannot slip
  a private address in. The refusal is
  `HttpError::InvalidRequest("blocked by the SSRF policy: …")`, which is not retried.
- **accepts `http` and `https` only.**
- **follows no redirect:** a `3xx` comes back as the response (reqwest followed up to 10 before).

No shim for the defaults (a guard that is off unless asked is what M3 found); the opt-out is
`dangerous_allow_private_networks()`. The caller-built-client entry points keep deprecated shims
(below).

Breaks and what to do:

- **Calls to internal services and local fakes fail.** Allowlist them, ideally from config:
  `HttpClientConfig::default().allow_hosts(["sendgrid.internal"])`, `.allow_cidrs([..])`
  (`IpCidr` parses `"10.20.0.0/16"`), or deserialize a `SsrfPolicy` (`allow_hosts`,
  `allow_cidrs`) from a config section and pass it with `.with_ssrf(policy)`. Tests against
  wiremock use `allow_hosts(["127.0.0.1"])` (wiremock binds loopback). A listed name skips the
  address check; listing a name does not open its literal address.
- **Code relying on redirects:** `HttpClientConfig::default().follow_redirects(max)` follows up
  to `max` hops and checks each hop's URL and resolved addresses like the first request.
- **Struct literals of `HttpClientConfig`** need the new fields `ssrf` and `max_redirects` (or
  `..HttpClientConfig::default()`).
- **No guard at all** (a trusted internal network): `.dangerous_allow_private_networks()` turns
  the range check off and records `id_effect_platform::http::SsrfPolicy::dangerous_allow_private_networks`
  in the posture registry, which the prod posture check refuses unless the risk is accepted.

Behind an HTTP proxy the proxy resolves names, so only literal-IP URLs (refused before the proxy
is contacted) and a proxy given by host name are checked; a proxy configured as a literal IP is
used as given. JWKS fetching keeps its own client and rules (https only, no redirects,
1 MiB; 0.10.1 M15).

The reqwest helpers (`id_effect_platform::http::reqwest`) and caller-built clients:

- **`ReqwestClientLive::layer()` and `provide_reqwest_pool(n, ttl)`** built `reqwest::Client::new()`
  (10 redirects, no timeout, no guard). They now build from `HttpClientConfig::default()`: 10 s
  connect / 30 s total timeouts, no redirects, and the SSRF check in the client's connector
  (literal IPs) and DNS resolver (names). Their new `ReqwestClientLive::layer_with(config)` /
  `provide_reqwest_pool_with(n, ttl, config)` take the allowlist (`allow_hosts(["127.0.0.1"])`
  for wiremock, `allow_hosts(["localhost"])` for a local fake reached by name) and redirect
  limit; `ReqwestClient::from_config` / `PooledClient::from_config` build one client. A refused
  request fails with a `reqwest::Error` whose source chain holds the `SsrfBlocked`.
- **`provide_reqwest_client(client)` and `ReqwestHttpClient::new(client)` are deprecated** (L9
  shims, removed in 0.12). They keep wrapping your client as is, and now record the opt-out.
  Rename to `dangerous_provide_reqwest_client(client)` /
  `ReqwestHttpClient::dangerous_unguarded(client)` to keep your own client (posture ids
  `id_effect_platform::http::reqwest::dangerous_provide_reqwest_client` and
  `id_effect_platform::http::ReqwestHttpClient::dangerous_unguarded`), or switch to
  `ReqwestClientLive::layer_with(config)` / `ReqwestHttpClient::from_config(&config)`.
  `ReqwestClient(client)` and `PooledClient::new(client)` still wrap a client as is (unrecorded).
- **`id_effect_auth` OIDC** (`OidcClient::discover`): the issuer and the token, user-info and
  JWKS endpoints the provider publishes must be `https`; plain `http` is accepted only to
  `localhost` or a loopback address (tests, a local Keycloak) — the JWKS rule of 0.10.1. A
  discovery document that names a plain-http endpoint fails with `OidcError::Discovery`, a
  plain-http issuer with `OidcError::Config`. A provider reached over a trusted encrypted
  network (a mesh with mTLS) takes `OidcConfig::insecure_allow_http()`, recorded as
  `id_effect_auth::oidc::OidcConfig::insecure_allow_http`. Provider requests now also have a
  connect timeout (`oidc::DEFAULT_CONNECT_TIMEOUT`, 10 s, capped by `OidcConfig::timeout`);
  redirects stay unfollowed.
- **`JwksClient::allow_http_dangerous()` is deprecated** (L9 shim, removed in 0.12): rename to
  `JwksClient::insecure_allow_http()`. Both now record
  `id_effect_auth::jwks::JwksClient::insecure_allow_http` (`jwks::INSECURE_ALLOW_HTTP`) in the
  posture registry, so the prod posture check refuses it unless the risk is accepted.

## Mesh (`id_effect_node`)

### Node: mesh protocol version

`PROTOCOL_VERSION` is 3 (0.10.1 spoke 2, 0.10.0 spoke 1). A 0.11 node refuses a 0.10.x peer in
the handshake with `HandshakeError::VersionMismatch` (and tells it why); there are no mixed-version
clusters (ADR 0009 non-goal). Upgrade every node of a mesh together: stop the 0.10 nodes, start the
0.11 ones. No shim: a 0.11 node does not speak versions 1 and 2, so the minimum peer of a 0.11
node is 0.11.

What the version adds, and what changes for code:

- `ProcessEnvelope` gains `Reliable` and `Ack` (at-least-once, see below). An exhaustive `match`
  over it needs arms for them (or a `_` arm).
- `ProcessEnvelope::Cast`, `Info` and `Call` now carry the sending fiber's trace context when they
  are sent by an effect (`Mesh::cast`, `Mesh::send`, the `*_reliable` sends, `Mesh::remote_call`).
  The plain functions `Mesh::remote_cast` and `Mesh::remote_info` run outside any fiber and still
  send an empty context; switch to `Mesh::cast` / `Mesh::send` to propagate traces. The default
  propagator, `W3cTraceContext`, writes a `traceparent` from id_effect span ids and opens no
  receiving span; give it a hook (`with_receiver_hook`) or install your own `TracePropagator`
  (for OpenTelemetry: inject `span_otel_context`, return `OtelSpanHook::with_parent(..)`) with
  `Mesh::with_trace_propagator`.
- Delivery stays at-most-once by default. To opt in, send with `Mesh::send_reliable` /
  `Mesh::cast_reliable`, or build the mesh with
  `Mesh::with_delivery(DeliveryConfig::at_least_once())`, which makes `remote_cast`,
  `remote_info`, `cast` and `send` reliable too. Messages may then arrive twice after a sender
  restart: make their handlers idempotent. Register `Mesh::monitor_delivery` to learn about
  messages given up (`DeliveryFailure::BufferFull`, `DeliveryFailure::PeerRestarted`), and
  connect sessions with `Mesh::add_peer_node(session.peer(), sender)` instead of `add_peer` so a
  restarted peer is recognised before anything is resent.

### Node: QUIC requires client certificates (mutual TLS)

Transports are mutually authenticated by default (ADR 0013 R8; dist.tls #4). Book chapter
"Transport security: mutual TLS". No shim; the explicit opt-out is
`QuicConfig::dangerous_no_client_auth()`.

- **A QUIC listener verifies the dialer's certificate.** Every dial presents the node's own
  certificate; the listener refuses a peer without one, or with one it does not trust, during the
  TLS handshake (that connection's `Listener::accept` returns `TransportError::Connect`).
- **`listen` needs client trust**, or fails with `TransportError::Bind` naming the new
  `QuicConfigError::NoClientTrust` (`QuicConfigError` is `#[non_exhaustive]`). Pick one:
  - CA roots: `with_client_verification(roots)` now verifies the clients a node accepts as well as
    the servers it dials (`with_client_cert_roots` sets other roots for clients). Configs built
    with roots in 0.10 keep working if the node certificates chain to those roots; issue them with
    the `clientAuth` extended key usage if they carry an EKU extension at all.
  - Pins: replace `with_pinned_server_cert_sha256(fp)` with `with_pinned_peer_cert_sha256(fp)` and
    pin every node's certificate on every node (or add `with_pinned_client_cert_sha256` on the
    listener for each dialer).
  - Opt out: `dangerous_no_client_auth()` restores the 0.10 listener; it records the posture
    opt-out `id_effect_node::QuicConfig::dangerous_no_client_auth`.
- `dangerous_accept_any_server_cert()` now records the posture opt-out
  `id_effect_node::QuicConfig::dangerous_accept_any_server_cert`.
- New: `transport::certs` (`NodeCertificate::from_pem_files` with PKCS#8, PKCS#1 and SEC1 keys,
  `load_ca_bundle`, `CertReloader`), `QuicConfig::from_certificate` and
  `QuicConfig::from_reloader` (a rotated certificate reaches new connections without a restart).
- New features of `id_effect_node`, used by later 0.11 changes: `tls-tcp`,
  `insecure-plaintext-tcp`, `kubernetes`. Its `rcgen` dependency uses aws-lc-rs instead of ring.

### Node: plaintext TCP needs the `insecure-plaintext-tcp` feature

TCP is encrypted and mutually authenticated by default (ADR 0013 R8; dist.tls #2). `TcpTransport`
is no longer a unit struct and no longer `Copy` (it is `Clone`): the 0.10 expression
`TcpTransport` (and `TcpTransport::default()`) no longer compiles, a transport used twice is
`.clone()`d, and without one of the features below the type has no constructor. No shim: a unit
struct cannot carry the TLS configuration, and keeping plaintext constructible by default is
what 0.11 removes.

- **Move to TLS (recommended).** Enable `tls-tcp` and build the transport from certificates, with
  the trust options of the QUIC transport:

  ```rust,ignore
  use id_effect_node::transport::certs::{NodeCertificate, load_ca_bundle};
  use id_effect_node::transport::tls_tcp::TlsTcpConfig;

  let cert = NodeCertificate::from_pem_files("/etc/mesh/node.crt", "/etc/mesh/node.key")?;
  let config = TlsTcpConfig::from_certificate(&cert)?
    .with_client_verification(load_ca_bundle("/etc/mesh/ca.pem")?);
  let tcp = TcpTransport::try_tls(config)?; // was: TcpTransport
  ```

  Every node needs a certificate valid for the server name (`"cluster"` by default,
  `with_server_name`) with the `serverAuth` and `clientAuth` usages, or pin each node's
  fingerprint with `with_pinned_peer_cert_sha256`. A dial without server trust fails with
  `TlsTcpConfigError::NoServerTrust`, a `listen` without client trust with `NoClientTrust`. Both
  ends of a session must use TLS: a TLS node and a 0.10 plaintext node cannot connect. A refused
  or timed-out TLS handshake is a `TransportError::Connect` from `Listener::accept`; keep
  accepting.
- **Keep plaintext (tests, trusted networks only).** Enable `insecure-plaintext-tcp` and replace
  `TcpTransport` with `TcpTransport::dangerous_plaintext()`. It records the posture opt-out
  `id_effect_node::TcpTransport::dangerous_plaintext`, so the prod posture check refuses to start
  until the operator accepts it.
- `TcpTransport::is_encrypted()` tells the two apart (e.g. for a posture probe).
- `Cookie::dangerous_allow_short()` now records the posture opt-out
  `id_effect_node::Cookie::dangerous_allow_short`.

### Node: global names (`ProcessEnvelope` gains variants)

`Mesh::global()` returns the new cluster-wide name registry (`GlobalRegistry`, book chapter
"Cluster-wide names"). Nothing changes unless you match on `ProcessEnvelope` or mix versions:

- `ProcessEnvelope` gains `GlobalRegister`, `GlobalUnregister` and `GlobalSync`. An exhaustive
  `match` over it needs arms for them (or a `_` arm).
- A 0.10 node cannot decode these envelopes. Do not run 0.10 and 0.11 nodes in one mesh; the
  protocol version check ("Node: mesh protocol version") refuses the mix.
- `Mesh::add_peer` now sends the new peer the global names this node owns (a `GlobalSync`), but
  only when it owns at least one. Code that counts envelopes on a peer sender sees it only if
  it registers global names.

## Processes (`id_effect_process`)

### Processes: `Next`, `Info`, `ExitReason` and `SendError`

Every process mailbox is bounded now (ADR 0013 R4; finding M10, decision D17). No shim: there is
no unbounded mailbox to fall back to, and `#[non_exhaustive]` cannot be phased in.

- **Mailboxes hold at most 10,000 user messages** (`DEFAULT_MAILBOX_CAPACITY`). Set another bound
  per process with `ProcessNode::spawn_with(args, env, SpawnOptions::new().mailbox_capacity(n))`
  or `spawn_linked_to_with`; `Mailbox::new()` is bounded at the default, `Mailbox::with_capacity`
  takes a bound. There is no unbounded mailbox. Exit signals and monitor downs bypass the bound
  (they enter through `Mailbox::offer_system_now`), and a kill never queues.
- **`SendError` is `#[non_exhaustive]` and has a new variant `Full`.** `Addr::cast` and
  `Addr::send` fail with `SendError::Full` when the target's mailbox is full; `SendError::Rejected`
  now means only "mailbox closed" (its message reads `message rejected: mailbox closed`). A
  `match` on `SendError` needs a `Full` arm or a wildcard.
- **`CallError` is `#[non_exhaustive]` and has a new variant `MailboxFull`**: `Addr::call` to a
  full mailbox fails with it instead of enqueueing.
- **`Addr::send_now` and `Mailbox::offer_now` return `false` for a full mailbox** as well as a
  closed one. `Mailbox::try_offer_now` says which (`OfferError::Full` or `OfferError::Closed`).
- **Remote delivery respects the bound** (`id_effect_node`): a remote call to a full mailbox fails
  with the new `RemoteFault::MailboxFull`; a remote cast or info to a full mailbox is dropped and
  counted (`BehaviorRegistry::dropped_full`). A `match` on `RemoteFault` needs the new arm.
- A process that relied on an unbounded queue (a burst producer with no back-pressure) either
  handles `SendError::Full` (retry later, shed, or reply "busy") or spawns with a larger bound.

New: `ProcessCtx::receive::<Self, _>(matcher, timeout)` is a selective receive. It takes the first
message `matcher` accepts and leaves every skipped message queued, in arrival order, for the
normal loop; it fails with `ReceiveError::Timeout` past the deadline, `ReceiveError::ScanLimit`
once a capacity's worth of messages was skipped, and `ReceiveError::Closed` when the process is
being killed.

`Next`, `Info` and `ExitReason` are `#[non_exhaustive]` and gain a variant each (fault.actors #6,
#8; book chapter "Timers, selective receive and hibernate"):

- **`Next::Hibernate(state)`**: the loop runs the new provided `Process::hibernate` hook (default:
  keep the state), releases the mailbox buffer and resumes on the next message. Code that only
  builds `Next::Continue` / `Next::Stop` compiles unchanged; a `match` on a `Next` outside
  `id_effect_process` needs a wildcard arm.
- **`Info::Timeout`**: delivered to `handle_info` by the new idle timeout
  (`ProcessCtx::set_idle_timeout(d)`, once per idle period). It only arrives if you set one, but
  an exhaustive `match` over `Info<_>` no longer compiles (`non-exhaustive patterns: _ not
  covered`): add `_ => {}` or an `Info::Timeout` arm plus a wildcard.
- **`ExitReason::NameConflict(name)`** (`Display`: `name conflict: <name>`): the exit reason of a
  process that loses a cluster-wide name (`id_effect_node::name_conflict`, recognised by
  `conflicting_name`); test for it with `conflicting_name(&reason)` or a `NameConflict` arm, not
  by string. Breaking for 0.10 code: an exhaustive `match` on `ExitReason` needs a wildcard arm.
  On the wire it is the appended `ExitReasonWire` variant `NameConflict` (earlier variants keep
  their tags); a reason a wire version cannot name travels as `Error(<display>)`.
  `ExitReasonWire` (`id_effect_node`) is not `#[non_exhaustive]`, so an exhaustive `match` on it
  needs a `NameConflict` arm.

New: `ProcessCtx::send_after::<Self>(msg, d)` and `Addr::send_after(msg, d)` return a
`TimerRef`; `ProcessCtx::cancel_timer(timer)` / `TimerRef::cancel()` returning `true` means the
message is never delivered. Timers run on the executor clock, die with their process and count
against the mailbox bound when they fire (a full mailbox drops them). `Mailbox::allocated()` and
`Mailbox::shrink_to_fit()` expose the buffer that hibernating releases.

## STM (`id_effect`, `id_effect_fp`)

### STM: `id_effect_fp::stm` moves to `id_effect::stm`

Software transactional memory is a core module again (D5): `id_effect::stm` exposes `Stm`,
`TRef`, `TMap`, `TQueue`, `TSemaphore`, `Txn`, `Outcome`, `commit` and `atomically`. It is always
compiled (no feature, no new dependency) and its semantics are unchanged, including the global
commit lock. The items are not re-exported at the `id_effect` root.

`id_effect_fp::stm::*` and the root re-exports `id_effect_fp::{TRef, commit, …}` still work in
0.11 as `#[deprecated(note = "use id_effect::stm")]` aliases of the core items (the same types,
so values pass freely between old and new paths). They are removed in 0.12.

The types are type aliases now, so `cargo semver-checks` reports `Stm`, `TRef`, `TMap`, `TQueue`,
`TSemaphore`, `Txn` and `Outcome` as removed from `id_effect_fp`. Naming them, calling their
methods and writing `Outcome::Variant` in expressions and patterns compile unchanged; importing
variants through the alias (`use id_effect_fp::stm::Outcome::*`) does not, since Rust cannot
`use` through a type alias: import from `id_effect::stm` instead.

```rust,ignore
// 0.10
use id_effect_fp::{Stm, TRef, commit};
// 0.11
use id_effect::stm::{Stm, TRef, commit};
```

If `id_effect_fp` was only a dependency for STM, drop it. The STM examples (`077`–`086_stm_*`) and
the contention test moved to `crates/id_effect`: `cargo run -p id_effect --example 077_stm_tref`.

New in `id_effect::stm` (additive, no migration): `TSet`, `TArray`, `TPriorityQueue`, `TPromise`
and `TReentrantLock` (book chapter "Transactional Collections", examples `087`–`091_stm_*`). They
exist only in core; `id_effect_fp` does not alias them.

## Kit and templates (`id_effect_kit`, `id_effect_tool`)

### Kit and templates: the prod posture check

`id_effect_kit` gains the startup posture check of ADR 0013 R10 and new features; the
`id-effect new` templates run the check and deny undeclared routes.

- **New `id_effect_kit::posture`.** `Posture::load(&Profiles::new())` reads the profile and the
  accepted risks of its configuration; the app states what it serves (`.console(Surface::…)`,
  `.docs(Surface::…)`, `.security_layer(&layer)` or `.csrf(bool)`, `.host(&HostConfig)`,
  `.tcp_transport(&transport)` or `.no_mesh()`, …); `.check()` adds every opt-out the posture
  registry recorded and returns a `PostureReport` with nine named probes. `report.enforce()` is
  an error under `prod` (any profile but `dev` and `test`) when a probe failed; call it before
  serving and exit on the error. An opt-out runs in `prod` only when the profile's TOML lists
  it with a reason:

  ```toml
  [security.accepted_risks]
  "id_effect_axum::HostConfig::dangerous_no_load_shedding" = "the proxy caps concurrency; reviewed 2026-10"
  ```

  A blank reason does not accept it. Nothing changes for an app that does not call the check.
- **Kit `cluster` now enables `id_effect_node/tls-tcp`**, so `kit::node::TcpTransport::tls` and
  `TlsTcpConfig` exist through the kit (0.10 kit users had a `TcpTransport` they could not
  build: plaintext TCP needs id_effect_node's `insecure-plaintext-tcp` feature and is a recorded
  opt-out). New features `mesh-mtls` (the QUIC transport), `kubernetes`
  (`KubernetesDiscovery`) and `workflow` (`kit::workflow`, on top of `sql` and `jobs`); `full`
  includes the three.
- **Prelude:** `stm` (the module), `Public`, `Guarded`, `RouteGuard` and `LoadShedLayer` are new
  glob imports; a local item of the same name now shadows them, an explicit `use` still wins.
  `id_effect_axum` re-exports `LoadShedLayer` at its root.
- **Plain Axum routes next to typed ones.** `Router::into_axum()` covers only the typed router's
  routes. To deny an undeclared plain route too, declare the plain routes as `RouteInfo`s with
  their `access` and apply `AccessLayer::new(typed.routes().chain(&plain))` with `route_layer`
  after the last route; the service template's `app()` does this (`plain_routes()`).
- **Templates.** A project generated by 0.11 runs the posture check in `main.rs` (service,
  worker, cluster). The service's `GET /` is a typed public route and undeclared routes answer
  `404`; `.env.example` documents `ID_EFFECT_PROFILE`, `METRICS_TOKEN`, `MAX_IN_FLIGHT` and
  (with `--console`) `CONSOLE_ALLOWED_HOSTS`. In `prod` the console answers `401` until real
  authentication sits in front of its guard. The cluster template builds its mesh transport as
  TLS TCP with mutual authentication from `MESH_CA_PEM`, `MESH_CERT_PEM` and `MESH_KEY_PEM`
  (throwaway dev certificates when none is set in `dev`/`test`; `prod` refuses to start without
  them). Existing generated projects are not changed; copy `posture()` and the `main.rs` check
  from a freshly generated project to adopt them.
- **`scripts/check-dangerous.sh`** (CI, lint workflow) fails when a function named
  `dangerous_*`, `insecure_*`, `*_dangerous` or `*_insecure` under `crates/` does not call
  `record_dangerous` (directly or through another such function).

## Crates promoted from `labs/` (`id_effect_workflow`, `id_effect_lints`)

### Workflows: `id_effect_workflow` replaces the labs crate

`id_effect_workflow` is a new published crate (tier platform): durable workflows journaled in
Postgres through `id_effect_sql` (decisions D6, D7). Nothing published before 0.11 changes; this
section is for users of the unpublished `labs/id_effect_workflow`.

- **No duroxide, duroxide-pg, SQLite or `NetworkJournalStub`.** The store is `PgWorkflowStore`
  over any `SqlClient` (tables `workflow_instances`, `workflow_history`); run
  `id_effect_workflow::migrator()` at startup (it tracks its versions in
  `_id_effect_workflow_migrations`) or copy `migrations/0001_id_effect_workflow.{up,down}.sql` into
  your own migrations. `MemoryWorkflowStore` replaces the in-memory journals in tests. Journals
  written by the labs crate are not read; finish or drop those instances before switching.
- **A workflow is a type implementing `Workflow`** (`NAME`, `VERSION`, schema-encoded `Input` and
  `Output`, `run(ctx, input)`), registered with `WorkflowEngine::register` and started with
  `engine.start::<W>(WorkflowId, input)` (idempotent per id). Read it back with `status`, `result`
  and `instance`; drive it with `engine.run()` (forked; any number of engines per database),
  `run_once` or `resume`.
- **Side effects go through `ctx.step(name, effect)`**, time through `ctx.now()` and randomness
  through `ctx.random()`; each is journaled and replayed by position and name. A replay that asks
  for another step than the history holds fails the instance with
  `WorkflowError::NonDeterministic`, so renaming or reordering steps of a running workflow needs a
  new workflow name until versioned routing ships (E13.5). Step results must be `#[derive(Schema)]`
  types (`HasSchema<A = Self, I = Unknown, E = ()>`), like job payloads.
- **Activities are `id_effect_jobs` jobs.** Implement `Activity` (`NAME`, `Input`, `Output`,
  `run(ctx, input) -> Effect<Output, JobFailure>`, optional `QUEUE`, `MAX_ATTEMPTS`, `TIMEOUT`,
  `retry()`), call it from a body with `ctx.activity::<A>(input)`, give the engines a queue with
  `WorkflowEngine::with_job_queue(PgJobQueue)`, and add the handlers to the workers' registry with
  `engine.register_activity(registry, activity)`; the workers serve `ACTIVITY_QUEUE` (or the
  activity's `QUEUE`). The labs crate's in-process activity calls have no equivalent: a side
  effect is a `ctx.step` (in the engine) or an activity (on any worker).
- **Durable timers** are `ctx.sleep(d)` / `ctx.sleep_until(t)`: one job on `TIMER_QUEUE` with
  `run_at`; add `engine.register_timers(registry)` and serve `TIMER_QUEUE` on the workers.
- **Signals**: `engine.client().signal(&id, name, payload)` (or `WorkflowClient::new(store)`) and
  `ctx.wait_signal::<P>(name)` in the body; signals sent before the wait are buffered.
- Run the crate's `migrator()` again: migration `0002_activities_timers_signals` adds
  `workflow_instances.waiting` / `wakeups`, `workflow_completions` and `workflow_signals`, and
  replaces the claim index. Run `id_effect_jobs::migrator()` too (the job table). Custom
  `WorkflowStore` implementations gain `wait`, `completion`, `complete`, `signal` and
  `next_signal`; `InstanceRecord` gains `waiting` and `wakeups`, `Claimed` gains `wakeups`.
- A body that catches `WorkflowError` must let `Waiting` (the run parked) and `LeaseLost` through;
  a caught `NonDeterministic` still fails the instance.
- **Sagas**: `ctx.saga(|saga| body)` with `saga.activity::<A, C, _>(input, undo)` or
  `saga.compensate::<C>(input)`; compensations are activities, journaled (each registration is the
  step `compensate:<NAME>`), and run in reverse order when the body fails, across restarts.
  `id_effect_fsm::Saga` is unchanged and stays the in-memory saga.
- **The FSM bridge is gone** (`id_effect_workflow::fsm::{register_fsm, step_durable,
  restore_state}`, `StepJournal`, `DurableWorkflowLog`, `DuroxideStepJournal`): drive a
  `StateMachine` from a workflow body instead, with events as signals and each transition in a
  `ctx.step` (book "Workflow bridge"). The labs examples `001_restart_resume` and
  `030_durable_door_fsm` are replaced by `010_order_saga`, `020_durable_timer` and
  `030_signal_approval` in `crates/id_effect_workflow/examples`.
- **Removed with the labs crate** (the package name is the same, so `cargo semver-checks` lists
  them): the features `duroxide` and `memory`; `bootstrap_duroxide_schema`,
  `provide_duroxide_pg`, `DUROXIDE_STEP_JOURNAL_DDL`, `DuroxideStepJournal`,
  `DuroxideWorkflowRuntime` and the SQLite `DurableWorkflowLog`; the journal transport
  (`StepJournal`, `DistributedStepJournal`, `JournalTransport`, `JournalServer`,
  `RemoteStepJournal`, `LoopbackTransport`, `NetworkJournalStub`, `DistributedJournalConfig`);
  and the `fsm` module (`register_fsm`, `step_durable`, `restore_state`, `FsmSnapshot`,
  `WorkflowFsmError`). The store is `PgWorkflowStore` / `MemoryWorkflowStore` behind
  `WorkflowStore`; there is no network journal. No shim: the crate was never published.
- **`WorkflowError` is `#[non_exhaustive]`** and its variants are new: `Sqlite`, `Postgres` and
  `Json` become `Sql(SqlError)` and `Decode { what, message }`; `InvalidWorkflowId` is
  `InvalidId`; `WorkflowAlreadyExists` is `IdInUse { id, workflow }` (starting the same id as the
  same workflow is idempotent, not an error); `Transport` is gone with the network journal. Match
  with a wildcard arm.

### Lints: `id_effect_lints` replaces the labs crate

The unpublished `labs/id_effect_lint` is now the Dylint library `id_effect_lints` at
`lints/id_effect_lints` (E1.9). Nothing published before 0.11 changes; this section is for users
of the labs crate.

- **Run it with `cargo dylint`** instead of loading the cdylib into a rustc driver yourself:
  `cargo install cargo-dylint dylint-link`, then name it in `[workspace.metadata.dylint]`
  (`{ git = "https://github.com/Industrial/id_effect", tag = "v0.11.0", pattern =
  "lints/id_effect_lints" }`) and run `cargo dylint --all`. Dylint builds it with the nightly in
  its `rust-toolchain.toml`; your toolchain does not change. Book appendix D has the details.
- **Every lint warns.** The labs crate declared 16 of them `Deny`; raise the ones you relied on
  with `DYLINT_RUSTFLAGS="-D <lint>"` or `#![cfg_attr(dylint_lib = "id_effect_lints",
  deny(<lint>))]`. A plain `#[allow(<lint>)]` is an unknown lint to ordinary `rustc`; wrap it the
  same way.
- **Detection fixes:** `#[test]` functions and `#[cfg(test)]` items are recognised (on current
  nightlies the labs crate's attribute lookup found neither, so the test-only lints never fired
  and the test exemptions never applied);
  `panic!`/`unreachable!`/`println!` in Effect code, `SystemTime::now()`, `#[thread_local]` and
  `thread_local!` are reported; `.await` inside `from_async(…)` and `static NAME: &str` are no
  longer reported. Expect new warnings where those lints were silent.
- **New lints:** `effect_map_identity`, `effect_flat_map_succeed` and `effect_flat_map_then`
  (fixed by `cargo dylint --fix`), and `raw_sql_non_literal` (see "SQL: `Sql` and `RawSql`").
- The ast-grep rules load again (one rule per YAML document, plus `utils/`); ids and severities
  are unchanged: `ast-grep scan --config lints/id_effect_lints/sgconfig.yml`. In the labs crate
  every rules file was a YAML list and most used keys ast-grep rejects, so no rule loaded; expect
  findings where the scan was silent. The lint workflow runs them in a new `ast-grep` job (report
  only), and `lints/id_effect_lints/examples/demo` is a small crate both the Dylint lints and the
  ast-grep rules fire on (`cargo dylint --all` from its directory).

## Performance (no API change)

The full secure stack now costs less than a bare request did before request fibers ran inline
(8.4 µs in the same bench). Nothing needs migrating; one behaviour is new.

- **Request fibers run inline.** `id_effect_axum`'s `EffectExecutor` drives each request's root
  fiber on the task polling the handler (or middleware) future instead of spawning a tokio task
  and awaiting it; a pinned `EffectExecutorBuilder::runtime` still spawns. Code inside a request
  effect therefore runs inside the caller's `tracing` span and task, as an `async` handler does.
  A fiber still goes to the executor when the caller drops the future early, and is interrupted
  there under `DropPolicy::Interrupt` (finalizers included), as before.
- **The secure stack was trimmed**: `SecurityLayer` renders the CSP header once per policy, keys
  the CSRF HMAC once per layer and masks the token only when it is read; an in-process API-key
  store and a pure guard policy decide inline (`ApiKeyStore::find_now`, `Policy::decide_now`,
  "Auth and authz: optional synchronous fast paths"); the logger reads no fiber ref that is not
  installed and formats no line `tracing` would drop. In the `gate_request` bench
  (`docs/benchmarks/README.md`, "Secure-stack budget (0.11)", 2026-10-19) the bare request takes
  0.91 µs and the full secure stack (rate limit, load shedding, `SecurityLayer::recommended`,
  API-key authentication, an `authenticated()` guard, default redaction) 3.54 µs, down from 5.42
  µs before the trim: +2.63 µs per request over bare.
- **The bench gate is an absolute budget**: `secure_stack − bare ≤ 5 µs` per request, measured in
  one run (owner decision 2026-10-19), instead of the ratio `secure_stack / bare ≤ 1.15`, which
  is printed for information only. With a 0.9 µs bare request a ratio gate would leave about
  135 ns for the security layers. Only `scripts/bench-compare.sh` users and `gates.toml` readers
  see this (`kind = "delta"`).

## `cargo semver-checks` against 0.10.1

`cargo semver-checks --workspace --baseline-rev v0.10.1` (cargo-semver-checks 0.47.0; rustdoc
from Rust 1.95, since 0.47 reads rustdoc JSON v56–v57 only) checked 29 crates: `id_effect`,
`id_effect_auth`, `id_effect_authz`, `id_effect_axum`, `id_effect_cli`, `id_effect_config`,
`id_effect_console`, `id_effect_events`, `id_effect_fp`, `id_effect_fsm`, `id_effect_jobs`,
`id_effect_kit`, `id_effect_logger`, `id_effect_macro`, `id_effect_mail`, `id_effect_messaging`,
`id_effect_node`, `id_effect_opentelemetry`, `id_effect_optics`, `id_effect_platform`,
`id_effect_process`, `id_effect_redis`, `id_effect_resilience`, `id_effect_sql`,
`id_effect_storage`, `id_effect_test`, `id_effect_tokio`, `id_effect_tool` and
`id_effect_workflow` (against the labs crate of the same name). Every break it reports is listed
above:

| Crate | Lint | Item | Entry |
|---|---|---|---|
| `id_effect_axum` | `method_requires_different_generic_type_params` | `route::Router::route` | Routing |
| `id_effect_axum` | `constructible_struct_adds_field` | `HostConfig.load_shed` | Web: load shedding |
| `id_effect_axum` | `auto_trait_impl_removed` | `OpenApiUi` no longer `UnwindSafe` / `RefUnwindSafe` | Web: `/docs`, `/metrics` and the console |
| `id_effect_console` | `derive_trait_impl_removed` | `ConsoleConfig` no longer `Copy` | Web: `/docs`, `/metrics` and the console |
| `id_effect_platform` | `constructible_struct_adds_field` | `HttpClientConfig.ssrf`, `.max_redirects` | HTTP client |
| `id_effect_node` | `unit_struct_changed_kind`, `derive_trait_impl_removed` | `TcpTransport` (not a unit struct, not `Copy`) | Node: plaintext TCP |
| `id_effect_node` | `enum_variant_added` | `ProcessEnvelope::{GlobalRegister, GlobalUnregister, GlobalSync}` | Node: global names |
| `id_effect_node` | `enum_variant_added` | `ProcessEnvelope::{Reliable, Ack}` | Node: mesh protocol version |
| `id_effect_node` | `enum_variant_added` | `RemoteFault::MailboxFull` | Processes |
| `id_effect_node` | `enum_variant_added` | `ExitReasonWire::NameConflict` | Processes |
| `id_effect_process` | `enum_marked_non_exhaustive` | `Next`, `Info`, `ExitReason`, `SendError`, `CallError` | Processes |
| `id_effect_fp` | `struct_missing`, `enum_missing` | `Stm`, `TRef`, `TMap`, `TQueue`, `TSemaphore`, `Txn`, `Outcome` (now deprecated type aliases) | STM |
| `id_effect_workflow` | `struct_missing`, `trait_missing`, `function_missing`, `module_missing`, `pub_module_level_const_missing`, `enum_missing`, `feature_missing` | the labs crate's duroxide, SQLite, journal-transport and `fsm` items | Workflows |
| `id_effect_workflow` | `enum_marked_non_exhaustive`, `enum_variant_missing` | `WorkflowError` | Workflows |

The other 22 crates report no break. Several listed breaks are outside what the tool sees: trait
bounds and removed generic impls (`Secret<T: Zeroize>`, `Secret`'s `PartialEq`), argument types
behind `impl Into<_>` (`Sql`), new type parameters with defaults (`Route`, `Endpoint`),
behaviour (redaction, SSRF, tenant match, mutual TLS, CSRF masking, Redis `clear`) and the wire
protocol. A break the tool reports later that is missing here is a bug in this guide.
