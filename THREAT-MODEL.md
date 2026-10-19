# Threat model

This is the threat model of the published id_effect crates (`crates/*`) and the way they are built
and released. It lists, per trust boundary, what an attacker can try (STRIDE), what stops it, which
rule of [ADR 0013 "Secure by default"](docs/adrs/0013-secure-by-default.md) (R1–R10) the mitigation
belongs to, and the test or CI gate that keeps it true. It also tracks every finding of the
2026-10-18 security review: closed, open (with the 0.11 leaf that owns it) or accepted (with the
reason).

Status is as of `release/0.11.0` at 6c0053aa (the 0.11 exit sweep, 2026-10-19; first written at
0f615124). Each release re-checks every row
(see [Maintenance](#maintenance)). To report a vulnerability, follow [SECURITY.md](SECURITY.md).

## Scope and method

- **In scope:** the published workspace crates, the `id-effect` service templates
  (`crates/id_effect_tool/templates/`), the Ledger reference app (`examples/reference/ledger`) as
  the end-to-end consumer, and the repository's CI and release workflows.
- **Out of scope:** `labs/` (unpublished), the other `examples/`, applications built on id_effect
  (they own their own threat model; this one describes the defaults they inherit), the operating
  system, the Rust toolchain and the hosting platform.
- **Method:** STRIDE (Spoofing, Tampering, Repudiation, Information disclosure, Denial of
  service, Elevation of privilege) for each trust boundary below. A threat is **mitigated** when
  the default path is safe and the unsafe behaviour survives only as a named `dangerous_*` /
  `insecure_*` opt-out that records itself in the posture registry (R10,
  `crates/id_effect/src/observability/posture.rs:34`); that is ADR 0013's definition of a closed
  finding.
- **Status legend:** *closed* (fixed, with a regression test named after the finding; the commit
  is given), *open* (not fixed yet; the 0.11 leaf that owns it is named), *accepted* (a recorded
  risk with its reason, re-checked each release). "v0.10.1" is the squash commit 05808e56 (#110,
  tag `v0.10.1`); its leaves were r101-client (1a747a41), r101-auth (507598b0), r101-mesh
  (6ba92d7c) and r101-http (bac12513) on `release/0.10.1`.

### Assets

Credentials and sessions (passwords, password hashes, TOTP secrets and recovery codes, session
ids, JWTs, OIDC tokens, API keys, the mesh cookie); application data in Postgres, MySQL, SQLite and
Redis; durable jobs and workflow histories; the availability of services built on id_effect; log
and metrics streams; and the integrity of the published crates.

### Actors

Anonymous internet clients; authenticated users (possibly hostile, possibly of another tenant); a
network attacker on the path between clients, nodes or services; a host on the cluster network
that is not a cluster member; operators; contributors and dependency authors; CI and the crates.io
registry.

### Trust boundaries

| # | Boundary | Untrusted side | Crates and files |
|---|---|---|---|
| B1 | HTTP edge | Every byte from a client socket | `id_effect_axum` (`server/**`, `middleware/**`, `client_ip.rs`, `server/security/**`) |
| B2 | Authentication | Credentials presented by a client | `id_effect_auth` (`basic`, `session`, `jwt`, `jwks`, `oidc`, `totp`, `password`, `throttle`) |
| B3 | Authorization | An authenticated (or anonymous) principal asking for a resource | `id_effect_authz` (`guard.rs`, `policy.rs`, `resource.rs`), `id_effect_axum` (`route/**`) |
| B4 | Mesh | Peers and the cluster network | `id_effect_node` (`transport/**`, `handshake.rs`, `session.rs`, `wire.rs`, `registry.rs`), `id_effect_process` (mailboxes) |
| B5 | Console and operator surfaces | Anyone who can reach the console, `/docs` or `/metrics` | `id_effect_console`, `id_effect_axum` (`openapi/ui.rs`, `metrics.rs`), the service template |
| B6 | Data: SQL, Redis, secrets and logs | Query text and values, stored data, log readers | `id_effect_sql`, `id_effect_redis`, `id_effect_config` (`secret.rs`), `id_effect_logger` |
| B7 | Outbound HTTP | The URLs a service is asked to call and the servers it calls | `id_effect_platform` (`http/**`), `id_effect_mail` (SendGrid), `id_effect_auth` (JWKS, OIDC discovery) |
| B8 | Supply chain | Contributors, dependencies, CI, the registry | `.github/workflows/**`, `deny.toml`, `Cargo.lock`, the publish workflow |

ADR 0009 ("Process mesh", non-goals) fixes one assumption that bounds B4: the cookie boundary
assumes a **trusted cluster**. Members that hold the cookie are trusted; the mesh does not defend
against a malicious member (no Byzantine fault tolerance). B4 defends the boundary between members
and everyone else.

## B1 — HTTP edge

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| S | A client spoofs its IP with `X-Forwarded-For` to dodge per-IP limits (H1, H2). | `ClientIp` reads `X-Forwarded-For` only when the socket peer is a `TrustedProxies` entry, from the right (`ClientIp::resolve`, `crates/id_effect_axum/src/client_ip.rs:165`); every serve path records `ConnectInfo` through `serve_listener` (`server/connection.rs:201`). | R4 | `h1_per_ip_limit_sees_the_peer`, `h1_forwarded_for_behind_a_trusted_proxy_keys_the_client` (`crates/id_effect_axum/tests/connection.rs:113`, `:137`); `h2_ledger_ignores_spoofed_forwarded_for` (`examples/reference/ledger/tests/authn.rs:319`) | closed (v0.10.1) |
| T | Cross-site request forgery on cookie-authenticated forms. | `SecurityLayer` CSRF check (`server/security/mod.rs:151`, `server/security/csrf.rs`); turning it off takes `SecurityLayer::dangerous_without_csrf()` (`mod.rs:218`), which records itself (ec818604; `without_csrf()` is a deprecated shim). | R9, R10 | `crates/id_effect_axum/tests/security_defaults.rs`; `csrf_opt_out_is_recorded` (`server/security/mod.rs:413`); posture probe `CSRF_ON` | mitigated |
| T, I | BREACH recovers the static CSRF token from compressed responses (L1); the token is not rotated at login (L5). | The token a page or the `X-CSRF-Token` header carries is masked with a fresh 32-byte pad per response and unmasked and compared with the cookie secret in constant time; `CsrfToken::rotate` (`server/security/csrf.rs:176`) issues a new secret and old tokens stop validating; `Sessions::login_with_csrf` (`crates/id_effect_auth/src/session/manager.rs:103`) logs in under a fresh session id and rotates the token. | R9 | `l1_csrf_token_masked_differently_per_response`, `l1_masked_token_validates`, `l5_rotate_issues_new_token_and_invalidates_old` (`tests/security_defaults.rs:176`, `:208`, `:230`); `l5_csrf_token_rotates_at_login`, `l5_session_id_rotates_at_login` (`crates/id_effect_auth/tests/auth_state.rs:123`, `:29`) | closed (5d804c5d, 7af82ba1) |
| I | Missing COOP, CORP and Permissions-Policy headers (L2, headers part). | `SecurityHeaders::recommended` (`server/security/headers.rs:71`) adds `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Resource-Policy: same-origin` and a `Permissions-Policy` denying camera, microphone, geolocation, payment and usb. | R9 | `l2_recommended_headers_include_coop_corp_permissions_policy` (`tests/security_defaults.rs:272`) | closed (5d804c5d) |
| R | A request cannot be traced to a log line. | Request ids (`middleware/request_id.rs`); escaped log lines (B6). | R3 | `m12_*` (B6) | mitigated |
| D | Slowloris, connection floods, unbounded keep-alive (M1). | `ConnectionLimits` (`server/connection.rs:65`): 10 s header-read timeout, 10,000 connections, 1,000 requests per connection; opt-outs `dangerous_no_header_read_timeout`, `dangerous_unlimited_connections`, `dangerous_unlimited_requests_per_connection` (`:115`, `:140`, `:173`), each recorded in the posture registry (38042a63). | R4, R10 | `m1_slow_headers_are_cut_off`, `m1_connection_cap_holds`, `m1_keep_alive_limit_closes_the_connection` (`tests/connection.rs:168`, `:214`, `:245`); posture probe `LOAD_SHEDDING_ON` | closed (v0.10.1; opt-outs recorded 38042a63) |
| D | Oversized bodies. | `BodyLimitLayer` (`middleware/limits.rs:138`). | R4 | `crates/id_effect_axum/tests/limits.rs` | mitigated |
| D | The template CSRF layer buffers a 2 MiB form outside the limit and timeout layers (M4). | The CSRF form read happens only for urlencoded bodies, up to `CsrfConfig::form_limit` (default 64 KiB, `server/security/csrf.rs:65`; a declared or read body over it gets 413, `:389`) and within `CsrfConfig::read_timeout` (default 10 s, `:68`; a stalled body gets 408), whatever the layer order. The service template also puts `BodyLimitLayer` and `TimeoutLayer` outside `SecurityLayer`. | R4 | `m4_csrf_form_read_respects_limit`, `m4_csrf_form_read_times_out` (`tests/security_defaults.rs:115`, `:160`); `m4_template_limit_layers_wrap_security_layer` (`crates/id_effect_tool/tests/template_security.rs:34` and the generated service's `tests/http.rs`) | closed (5d804c5d layer, d13d4d94 template) |
| D | No concurrency cap or load shedding (M5). | `LoadShedLayer` (`middleware/load_shed.rs:120`) caps in-flight requests and answers 503 Problem Details with `Retry-After` at once (no queue). `HostConfig` installs it on every serve path by default (512 per available CPU, env `MAX_IN_FLIGHT`); opt-out `HostConfig::dangerous_no_load_shedding()` (`server/bootstrap.rs:66`), recorded. | R4, R10 | `m5_load_shed_rejects_over_capacity_with_503`, `m5_default_server_stack_has_concurrency_cap` (`tests/limits.rs:55`, `:148`); posture probe `LOAD_SHEDDING_ON` | closed (8141e0f9) |
| D | Rate-limit eviction scans every bucket under one mutex (M8). | `MemoryRateLimitStore` (`middleware/rate_limit.rs:98`) is sharded by key hash; each shard is an LRU list behind its own mutex, so lookup and eviction are O(1) and never scan every key (`DEFAULT_MAX_KEYS` stays 100,000). | R4 | `m8_eviction_does_not_scan_all_keys` (`middleware/rate_limit.rs:680`), `m8_eviction_evicts_least_recently_used`, `m8_sharded_store_under_contention` (`tests/limits.rs:193`, `:211`) | closed (8141e0f9) |
| E | A route without a guard serves anonymous callers (H8). | See B3. | R1 | B3 | closed (cb54bdae) |

## B2 — Authentication

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| S | Online password guessing (H5). | `LoginThrottle` (`crates/id_effect_auth/src/throttle.rs:323`): 5 free failures per account and per client IP, then a doubling delay up to 15 min, before the user lookup and before hashing; opt-out `BasicAuth::dangerous_without_throttle` (`src/basic.rs:207`), recorded in the posture registry (c2ff95fa; the 0.10.1 name `without_throttle_dangerous` is a deprecated shim that delegates). | R4, R10 | `h5_throttle_checked_before_hashing`, `h5_dangerous_without_throttle_is_recorded` (`src/basic.rs:459`, `:494`), `h5_login_throttle_backs_off` (`tests/credentials.rs:311`), `h5_ledger_login_is_throttled` (`examples/reference/ledger/tests/authn.rs:275`) | closed (v0.10.1; opt-out recorded c2ff95fa) |
| S | TOTP brute force (H6). | Lockout after 5 wrong codes; opt-out `Totp::dangerous_without_lockout` (`src/totp.rs:527`), recorded (c2ff95fa; `without_lockout_dangerous` is the deprecated shim). | R10 | `h6_totp_locks_after_five_failures`, `h6_dangerous_without_lockout_is_recorded` (`src/totp.rs:1000`, `:1192`) | closed (v0.10.1; opt-out recorded c2ff95fa) |
| S | A TOTP code replayed on a second instance (M18): `MemoryTotpStepStore` remembers only the codes its own process accepted. | `PgTotpStepStore` (features `pg` + `totp`, `src/totp.rs:385`) shares the step state with one compare-and-set statement (migration `0003_id_effect_auth_state`). Creating a `MemoryTotpStepStore` (`src/totp.rs:290`) records `id_effect_auth::MemoryTotpStepStore::memory_only`, which fails the prod posture check. | R10 | `m18_pg_totp_replay_rejected_across_instances`, `m18_pg_totp_concurrent_consume_one_wins` (`tests/auth_state.rs:317`, `:371`), `m18_memory_step_store_is_reported_as_memory_only` (`src/totp.rs:1208`); posture probe `NO_MEMORY_ONLY_STATE` | closed (7af82ba1) |
| S | A JWT minted for another issuer or audience is accepted (H7). | A `JwtVerifier` with neither issuer nor audience rejects every token (`AuthError::Config`); opt-outs `dangerous_any_issuer` (`src/jwt.rs:491`) and `dangerous_any_audience` (`:516`), recorded (c2ff95fa; `any_issuer_dangerous` / `any_audience_dangerous` are the deprecated shims). | R10 | `h7_jwt_without_issuer_or_audience_is_rejected`, `h7_dangerous_opt_out_is_explicit`, `h7_dangerous_any_issuer_and_audience_are_recorded` (`src/jwt.rs:1200`, `:1219`, `:1237`) | closed (v0.10.1; opt-outs recorded c2ff95fa) |
| S, T | Forged tokens through a weak HMAC key or a JWKS served over plain http or a redirect (M15). | HMAC secrets under 32 bytes fail; JWKS over https only (loopback excepted), no redirects, 1 MiB cap; opt-out `JwksClient::insecure_allow_http` (`src/jwks.rs:128`), recorded (7d217442; the 0.10.1 name `allow_http_dangerous` is the deprecated shim). | R6, R10 | `m15_short_hmac_key_rejected` (`src/jwt.rs:1255`), `m15_jwks_requires_https`, `m15_jwks_insecure_allow_http_is_recorded` (`src/jwks.rs:389`, `:420`), `m15_jwks_body_capped`, `m15_jwks_redirects_and_plain_http_are_refused` (`tests/jwks.rs:183`, `:207`) | closed (v0.10.1; opt-out renamed and recorded 7d217442) |
| I | `Session`'s derived `Debug` printed its attributes, including OIDC tokens (M14). | `Session` (`src/session/mod.rs:109`) has a hand-written `Debug`: the id prefix, the user, the attribute keys and the timestamps, never the attribute values. | R2 | `m14_session_debug_hides_attr_values_and_tokens` (`src/session/mod.rs:337`) | closed (745c80dd) |
| I | Passwords stay in memory after use (M13, password part). | `Password` (`src/password.rs:123`) is overwritten with zeroes on drop. | R2 | `m13_password_is_zeroized_on_drop` (`src/password.rs:704`) | closed (745c80dd) |
| D | Hashing as a CPU-exhaustion vector (M20, L4). | Bounded hashing pool (2 × parallelism, 5 s wait, `PasswordError::Busy` → 503); `BasicAuth` rejects passwords over 256 bytes before hashing. | R4 | `m20_hashing_is_bounded` (`src/password.rs:775`), `l4_long_basic_password_rejected` (`src/basic.rs:535`) | closed (v0.10.1) |
| R | Failed logins leave no trace. | Auth failures are typed (`AuthError`, e.g. `TooManyAttempts` → 429) and answered with a status code; the application logs them, with the request id, through the redacting and escaping logger (R3). There is no built-in authentication audit log. | R3 | `h5_*`, `m11_*`, `m12_*` | partly mitigated; audit log not planned for 0.11 |
| E | The login throttle is per process, so N instances allow N × the budget. | `PgThrottleStore` (feature `pg`, `src/throttle.rs:207`) implements the 0.10.1 `ThrottleStore` trait over the table `id_effect_auth_failures` (same migration), so `LoginThrottle` and the TOTP lockout share counts across instances: `LoginThrottle::new(PgThrottleStore::new(..))`. Residual: see [Residual risks](#residual-risks). | R10 | `pg_login_throttle_shared_across_instances` (`tests/auth_state.rs:244`) | closed (7af82ba1) |

## B3 — Authorization

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| E | Routes were open unless guarded (H8): an unauthenticated request continues as `Anonymous` (`crates/id_effect_auth/src/middleware.rs:10`, `crates/id_effect_authz/src/guard.rs:13`), and before 0.11 `Router::route` accepted any endpoint. | Typestate on the access decision: `Router::route` (`crates/id_effect_axum/src/route/mod.rs:697`) takes only an endpoint that declared `.public()` or `.guard(g)` (any `RouteGuard`, `route/access.rs:156`; `id_effect_authz::Guard` implements it), so an undecided route does not compile. `Router::route_unchecked` (`:713`) is a deprecated shim that records `id_effect_axum::route::Router::route_unchecked`. `into_axum` installs `AccessLayer`, which answers 404 Problem Details to a matched route that is not a registered public, guarded or unchecked route, unless `allow_unregistered_routes_dangerous()` (`:754`, recorded). A guarded typed route denies `Anonymous` with 401 before its policy. Residual: see [Residual risks](#residual-risks). | R1, R10 | `h8_route_without_access_decision_does_not_compile` (`crates/id_effect_axum/tests/ui.rs:28`; five more cases in `tests/security_compile_fail`), `h8_unregistered_axum_route_is_denied`, `h8_route_unchecked_is_deprecated_and_recorded` (`tests/route_access.rs:82`, `:169`), `h8_anonymous_reaches_only_public_routes` (`crates/id_effect_authz/src/guard.rs:554`), `h8_every_template_route_declares_access` (generated service `tests/http.rs`) | closed (cb54bdae; template 888ac28f) |
| E, I | Cross-tenant access when a policy forgets the tenant check (M19). | A tenanted resource requires the tenant match by default (`same_tenant`, `crates/id_effect_authz/src/resource.rs:130`); crossing tenants needs `dangerous_cross_tenant(reason)` (`src/policy.rs:283`). | R1, R10 | `m19_tenanted_resource_requires_tenant_match_by_default`, `m19_cross_tenant_requires_named_opt_out`, `m19_untenanted_resource_unaffected` (`crates/id_effect_authz/tests/tenancy.rs:71`, `:163`, `:211`); compile-fail `m19_cross_tenant_without_opt_out` | closed (0aa4186b) |
| R | Nobody can list which routes are public. | `Router::access_report()` (`route/mod.rs:766`) lists public and guarded routes and is logged at startup; public OpenAPI operations carry `security: []`. | R1 | `h8_access_report_lists_public_routes` (`tests/route_access.rs:193`) | closed (cb54bdae); a Ledger snapshot of its report is r11-w3-ledger's |

## B4 — Mesh

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| S | A man in the middle poses as a QUIC server (H3). | Dialing needs trusted roots, a pinned leaf SHA-256 (`QuicConfig::with_pinned_server_cert_sha256`, `crates/id_effect_node/src/transport/quic.rs:166`) or `dangerous_accept_any_server_cert` (`:198`), which records itself (b92af181). TLS TCP has the same rule (`transport/tls_tcp.rs:183`, `:210`). | R8, R10 | `h3_quic_without_roots_is_a_config_error`, `h3_dangerous_accept_any_is_explicit` (`crates/id_effect_node/tests/mesh_security.rs:81`, `:109`), `dangerous_accept_any_server_cert_is_recorded_in_posture` (`tests/transport_tls.rs:440`) | closed (v0.10.1; opt-out recorded b92af181) |
| S, T | A relayed, reflected or replayed handshake proof (H4). | The proof covers version, role, both node names and incarnations, both nonces and, on QUIC and TLS TCP, a TLS exporter secret (TLS TCP since ae51878e); protocol version 3 since be6af99d (`src/wire.rs:27`). | R8 | `h4_relayed_proof_fails`, `h4_relayed_proof_fails_without_a_channel`, `h4_v1_peers_are_refused_with_version_mismatch` (`tests/mesh_security.rs:146`, `:256`, `:429`) | closed (v0.10.1) |
| S | A peer without a client certificate joins; in 0.10 QUIC servers used `with_no_client_auth()` and nothing but the cookie authenticated a dialer. | QUIC and TLS TCP run TLS 1.3 with mutual TLS: the dialer always presents the node's certificate and the listener requires and verifies it, against CA roots or pinned fingerprints. There is no default trust: a listener without client trust refuses to bind (`QuicConfigError::NoClientTrust`). `with_no_client_auth` survives only behind `dangerous_no_client_auth()` (`transport/quic.rs:216`, `transport/tls_tcp.rs:226`), recorded. PEM certificates and `CertReloader` rotation in `transport/certs.rs`. | R8, R10 | `mtls_rejects_peer_without_client_cert`, `mtls_rejects_cert_from_other_ca` (`tests/transport_tls.rs:252`, `:302`), `tcp_tls_rejects_untrusted_peer`, `tcp_tls_opt_outs_are_recorded_in_posture` (`tests/transport_tcp_tls.rs:170`, `:274`); posture probe `MESH_ENCRYPTED` | closed (b92af181, ae51878e) |
| I, T | The TCP transport was plaintext (0.10 warned once). | `TcpTransport::tls` (feature `tls-tcp`) carries the frames in TLS 1.3 with mutual authentication and a channel-bound cookie proof. Plaintext TCP exists only with the feature `insecure-plaintext-tcp`, as `TcpTransport::dangerous_plaintext()` (`transport/tcp.rs:205`), recorded; without either feature `TcpTransport` has no constructor. The kit's `cluster` feature enables `tls-tcp` only, and the cluster template builds a mutual-TLS TCP transport (prod refuses to start without its certificates). | R8, R10 | `tcp_tls_mutual_auth_round_trip`, `plaintext_tcp_opt_out_is_recorded` (`tests/transport_tcp_tls.rs:137`, `:293`); compile-fail `r8_plaintext_tcp_without_feature` (`tests/security_compile_fail`); posture probe `MESH_ENCRYPTED` | closed (ae51878e; kit and template 888ac28f) |
| D | Pre-authentication resource use: slow handshakes, large frames, short cookies, small nonces (M9). | 5 s handshake timeout (`DEFAULT_HANDSHAKE_TIMEOUT`, `src/handshake.rs:48`), 4 KiB frames until authenticated (`PRE_AUTH_MAX_FRAME_BYTES`, `:45`, applied `:292`), cookies of at least 32 bytes (opt-out `Cookie::dangerous_allow_short`, `:116`, recorded since ae51878e), 128-bit nonces. | R4, R10 | `m9_handshake_times_out`, `m9_oversized_pre_auth_frame_rejected`, `m9_frame_cap_lifts_after_authentication`, `m9_short_cookie_rejected` (`tests/mesh_security.rs:316-390`), `m9_nonces_are_128_bit`, `dangerous_allow_short_is_recorded_in_posture` (`src/handshake.rs:920`, `:599`) | closed (v0.10.1) |
| D | Unbounded process mailboxes reachable from remote sends (M10). | Every mailbox bounded (default `DEFAULT_MAILBOX_CAPACITY` 10,000, `crates/id_effect_process/src/mailbox.rs:48`); local sends fail with `SendError::Full`; a remote call to a full mailbox faults with `RemoteFault::MailboxFull`; a remote cast or info to a full mailbox is dropped and counted (`BehaviorRegistry::dropped_full`, `crates/id_effect_node/src/registry.rs:74`), also under at-least-once delivery, where the drop is acknowledged ([ADR 0009](docs/adrs/0009-process-mesh.md), Amendment 1); system messages bypass the bound. | R4 | `m10_bounded_mailbox_rejects_when_full`, `m10_system_messages_bypass_bound` (`crates/id_effect_process/tests/mailbox_bounds.rs:88`, `:144`), `m10_remote_send_to_full_mailbox_faults` (`crates/id_effect_node/src/registry.rs:490`) | closed (052d969e) |
| D | At-least-once delivery (opt-in, be6af99d) lets a peer grow the sender's retransmit buffer or the receiver's dedup state. | At-most-once stays the default. The retransmit buffer is bounded per peer in count and bytes (`DEFAULT_MAX_UNACKED` 1,024, `DEFAULT_MAX_UNACKED_BYTES` 8 MiB, `crates/id_effect_node/src/delivery.rs`); when full, the oldest message is dropped and reported as `DeliveryFailure::BufferFull`. The receiver tracks at most `dedup_window` (default 4,096) sequence numbers beyond the last contiguous one per sender and neither delivers nor acknowledges a frame beyond it. | R4 | `retransmit_buffer_bound_faults_oldest`, `duplicates_suppressed_after_reconnect`, `at_most_once_remains_default` (`crates/id_effect_node/tests/reliable_delivery.rs:271`, `:245`, `:330`); fuzz target `mesh_envelope` | mitigated (be6af99d) |
| T | Malformed payloads from a peer. | Payloads are decoded through `id_effect::schema` at the wire boundary (ADR 0009, "Wire protocol"). Fuzz targets cover the schema decoder (`schema_json`), the wire frames (`mesh_frame`) and the process envelopes through the receiving node's dispatch, dedup and ack path (`mesh_envelope`) (`fuzz/fuzz_targets/`). | R4 | ClusterFuzzLite (`.github/workflows/cflite_pr.yml`, `cflite_batch.yml`) | mitigated |
| E | A cookie holder spawns registered behaviours or sends to any process. | By design (ADR 0009 trusted-cluster assumption): only behaviours registered in the binary can be spawned, with Schema-validated arguments; no code mobility. | — | accepted assumption (ADR 0009 non-goals) | accepted (design) |

## B5 — Console and operator surfaces

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| E | The console API is reachable without a guard. | `Console::router(guard)` is the only way to get its routes (`crates/id_effect_console/src/api.rs:142`). | R7 | `crates/id_effect_console/tests/{api,console,csrf}.rs` | mitigated |
| S, E | The template console treated every loopback request as an operator and checked neither Host nor Origin (DNS rebinding); the Ledger enabled the console in every profile (M6). | `ConsoleConfig` carries the allowed hosts (default: the bind address) and `check_host_and_origin` (`crates/id_effect_console/src/lib.rs:192`) answers 403 to another Host or Origin. The template's `local_operator` exists only in the `dev` profile (`crates/id_effect_tool/templates/service/src/console.rs.template`) and the console listener checks Host and Origin against `CONSOLE_ALLOWED_HOSTS`. The Ledger's `config/base.toml` sets `[console] enabled = false` (`:80`); only `dev.toml` and the tests' `test.toml` turn it on. | R7 | `m6_console_rejects_foreign_host_and_origin` (`crates/id_effect_console/tests/host_origin.rs:43` and the generated service), `m6_loopback_operator_only_in_dev` (`crates/id_effect_tool/tests/template_security.rs:80`); posture probe `CONSOLE_GUARDED` | closed (d13d4d94) |
| T | `/docs` loaded the Scalar bundle from jsDelivr at `@1` without Subresource Integrity (M7). | `DEFAULT_SCALAR_URL` pins `@scalar/api-reference@1.75.0` with `DEFAULT_SCALAR_INTEGRITY` (sha384) (`crates/id_effect_axum/src/openapi/ui.rs:84`); `OpenApiUi::router` (`:343`) mounts nothing outside the `dev` and `test` profiles unless `.guard(g)` or `.public_dangerous()` (`:228`, recorded). | R7 | `m7_scalar_bundle_pinned_with_integrity`, `m7_docs_disabled_in_prod_unless_guarded` (`crates/id_effect_axum/tests/openapi_ui.rs:299`, `:350`); posture probe `DOCS_CLOSED` | closed (d13d4d94) |
| I | The template served `/metrics` on the public router (L2, metrics part). | `metrics::guarded_metrics_router(guard)` (`crates/id_effect_axum/src/metrics.rs:170`); the service template serves `/metrics` behind a bearer-token guard (`METRICS_TOKEN`). | R7 | `l2_template_metrics_not_public` (`crates/id_effect_tool/tests/template_security.rs:60` and the generated service) | closed (d13d4d94) |
| R | An operator's runtime change (log level, job retry or cancel) is not attributable. | Console actions need the guard's principal; they are not written to an audit log. | R7 | `crates/id_effect_console/tests/api.rs` | partly mitigated; audit log not planned for 0.11 |

## B6 — Data: SQL, Redis, secrets and logs

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| T, E | SQL injection through a formatted query string (M16). | `query`/`execute` take `impl Into<Sql>` where `Sql: From<&'static str>`; raw SQL needs `RawSqlCapability`, provided only by `RawSqlCapability::layer_dangerous()` (`crates/id_effect_sql/src/raw.rs:125`, `:135`); Dylint lint `raw_sql_non_literal` on the `SqlClient` trait methods (`lints/id_effect_lints/src/lib.rs:477`). | R5, R10 | `m16_query_with_string_does_not_compile`, `m16_query_raw_requires_capability` (`crates/id_effect_sql/tests/ui.rs:27`, `:35`), `m16_literal_query_still_compiles` (`src/raw.rs:180`); four `m16_*` compile-fail cases (`tests/security_compile_fail`); `.github/workflows/lint.yml` | closed (055e68f0, 1f909018) |
| T, D | Redis `clear()` with an empty namespace deletes every key (M17; `namespace_glob`, `crates/id_effect_redis/src/client.rs:356`). | An empty namespace is an error; `clear` deletes only namespace keys. | R5 | `m17_clear_with_empty_namespace_is_an_error`, `m17_clear_only_deletes_namespace_keys` (`crates/id_effect_redis/tests/store_clear.rs:15`, `:42`) | closed (055e68f0) |
| I | Secrets were never zeroized (M13, secret part). | `Secret<T: Zeroize>` (`crates/id_effect_config/src/secret.rs:22`) zeroizes its value on drop; `SecretString` / `SecretBytes` aliases. | R2 | `m13_secret_is_zeroized_on_drop` (`crates/id_effect_config/tests/secret.rs:22`); compile-fail `m13_secret_of_non_zeroize_type` | closed (745c80dd) |
| I | `Secret` equality was not constant time (L3). | Equality only on `SecretString` / `SecretBytes`, through `subtle::ConstantTimeEq` (`secret.rs:86`, `:99`); the generic `PartialEq` is removed. | R2 | `l3_secret_eq_uses_constant_time_compare` (`tests/secret.rs:51`); compile-fail `l3_generic_secret_equality_removed` | closed (745c80dd) |
| I | Secrets in log fields (M11): `RedactionPolicy::new()` redacted nothing. | The default pipeline redacts common secret fields; an empty policy must be explicit: `RedactionPolicy::dangerous_empty()` (`crates/id_effect_logger/src/redact.rs:107`) and `dangerous_none()` (`:140`) record themselves (ec818604); `new()` is a deprecated shim that delegates (the unreleased `empty()` was removed, 6c0053aa). | R3, R10 | `m11_default_pipeline_redacts_common_secret_fields`, `m11_empty_policy_is_explicit` (`crates/id_effect_logger/tests/security.rs:70`, `:147`); compile-fail `deny_deprecated_redaction_policy_new`; posture probe `REDACTION_ON` | closed (72257fb6; opt-outs recorded ec818604) |
| R, T | Log forging: newlines, control and ANSI sequences, or crafted keys inject lines and fields (M12). | Messages, values and keys are escaped (`crates/id_effect_logger/src/pipeline.rs`). | R3 | `m12_newline_in_message_cannot_forge_a_line`, `m12_annotation_key_cannot_inject_fields`, `m12_control_and_ansi_sequences_are_escaped` (`tests/security.rs:181`, `:203`, `:226`) | closed (72257fb6) |
| T | A stale workflow engine appends to a history another engine owns. | Lease with a fencing token checked on every append (0.11.0 D7, `crates/id_effect_workflow`). | — | `stale_writer_append_is_rejected_by_fencing` (`crates/id_effect_workflow/tests/engine_pg.rs:360`) | mitigated (5d3fc364) |

## B7 — Outbound HTTP

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| E, I | Server-side request forgery to loopback, private ranges or cloud metadata, and redirects followed to them (M3). | `HttpClientConfig::default()` carries an `SsrfPolicy` (`crates/id_effect_platform/src/http/ssrf.rs:216`) that every client the crate builds enforces: literal-IP URLs are checked before sending; a DNS resolver checks every resolved address (one forbidden address refuses the name, DNS-rebinding safe); a connector layer refuses a literal-IP destination on the clients the `http::reqwest` helpers hand out (7d217442). Refused: loopback, unspecified, RFC 1918, 100.64/10, link-local (169.254.169.254, fd00:ec2::254), ULA, multicast, reserved and broadcast, IPv4-mapped and -compatible forms, NAT64 and 6to4 forms of forbidden IPv4. No redirects by default; `follow_redirects(max)` re-checks every hop. `allow_hosts` / `allow_cidrs` from config. Opt-outs `SsrfPolicy::dangerous_allow_private_networks()`, `ReqwestHttpClient::dangerous_unguarded`, `dangerous_provide_reqwest_client`, all recorded (9ca8ce57). OIDC: https issuer and published endpoints, no redirects, a connect timeout; `OidcConfig::insecure_allow_http()` recorded. Residual: see [Residual risks](#residual-risks). | R6, R10 | `m3_ssrf_blocks_loopback_private_and_metadata`, `m3_ssrf_checks_every_resolved_address`, `m3_ipv4_mapped_ipv6_is_blocked`, `m3_redirects_not_followed_by_default`, `m3_followed_redirect_to_private_ip_is_blocked`, `m3_allowlist_permits_named_host` (`crates/id_effect_platform/tests/ssrf.rs:54`–`:251`); `m3_reqwest_helpers_refuse_literal_metadata_ip`, `m3_reqwest_helpers_refuse_literal_ipv6_loopback`, `m3_reqwest_layers_refuse_names_resolving_to_loopback`, `m3_reqwest_layers_do_not_follow_redirects` (`src/http/reqwest.rs:584`–`:656`); `m3_oidc_discovery_redirect_is_refused`, `m3_oidc_token_endpoint_redirect_is_refused`, `m3_oidc_endpoints_must_be_https` (`crates/id_effect_auth/tests/oidc.rs:752`, `:772`, `:802`); posture probe `SSRF_ON` | closed (080a9acc, 9ca8ce57, 7d217442) |
| D | A hostile server streams an unbounded body (M2). | `max_body_bytes` (default 10 MiB, `crates/id_effect_platform/src/http/mod.rs:63`) enforced on a declared `Content-Length` before reading and while streaming. | R6 | `m2_content_length_over_cap_rejected_before_reading`, `m2_streaming_body_over_cap_stops_early`, `m2_body_exactly_at_cap_is_accepted` (`crates/id_effect_platform/tests/http_body_cap.rs:110`, `:148`, `:181`) | closed (v0.10.1) |
| D | A slow server holds a request open. | Client timeouts (10 s connect, 30 s total by default, `http/mod.rs`); the `http::reqwest` layers build from the same defaults since 9ca8ce57. | R6 | `crates/id_effect_platform` tests | mitigated |
| S | A spoofed JWKS or OIDC endpoint. | https only, no redirects (B2 M15; OIDC under M3). | R6 | `m15_*`, `m3_oidc_*` | closed (0.10.1, 9ca8ce57) |

## B8 — Supply chain

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| T | A moved action tag or a floating toolchain changes what CI runs; `curl \| bash` installers (S1). | Every `uses:` pinned by full commit SHA with a version comment (the one exception is the SLSA generator, a reusable workflow that verifies its own `@vX.Y.Z` ref); `dtolnay/rust-toolchain` pinned with an explicit toolchain, and `rust-toolchain.toml`; the moon `curl \| bash` installer and the unchecked mdBook download replaced by pinned releases checked against their sha256; Dependabot keeps the pins current. | R10 | `s1_every_action_pinned_by_sha` (`scripts/check-workflows.sh --self-test`); `scripts/check-workflows.sh` over every workflow in moon-ci | closed (a6a97637; `publish.yml` edd5e5b8) |
| E | Workflows ran with the default token permissions; Dependabot auto-merge approved its own pull requests (S2). | Top-level `permissions: contents: read` (or `{}`) in every workflow, write grants only on the jobs that need them; `dependabot-auto-merge.yml` approves nothing and enables auto-merge only for patch and minor updates, so GitHub merges after the required `CI Pass` check and a CODEOWNERS review. | R10 | `s2_every_workflow_least_privilege`, `s2_dependabot_does_not_self_approve` (`scripts/check-workflows.sh --self-test`) | closed (a6a97637) |
| S, E | A leaked long-lived `CARGO_REGISTRY_TOKEN` publishes a malicious crate (S3). | crates.io trusted publishing only: `rust-lang/crates-io-auth-action` exchanges the publish job's GitHub OIDC token for a 30-minute crates.io token, one per half of the publish order; no workflow reads a registry secret ([docs/RELEASING.md](docs/RELEASING.md), "crates.io authentication"). The secret itself is deleted by the owner after the first trusted publish (0.11.0 D15). | — | `grep -rn CARGO_REGISTRY_TOKEN .github` finds nothing; `publish.yml` | closed in the workflow (edd5e5b8); owner steps open: trusted publisher on every crate, then delete the secret ([docs/RELEASING.md](docs/RELEASING.md) "Owner steps" 3 and 7) |
| T | Provenance attested crates re-downloaded from crates.io, not the files CI built; `Cargo.lock` was not committed (S5). | `Cargo.lock` committed and CI builds `--locked` (a6a97637). The publish workflow runs `cargo package --locked`, signs SLSA Build L3 provenance over the built `.crate` files and SBOMs (slsa-github-generator), checks every uploaded `.crate` against its attested sha256, and a `verify` job downloads every crate from crates.io, compares the digest and runs `slsa-verifier` (0.11.0 D12, D13). | — | `s5_attested_digest_matches_published_crate` (`publish.yml`, `verify` job) | closed (a6a97637, edd5e5b8); first exercised by the 0.11.0 release |
| T | Memory-safety and parser bugs nobody looked for (S4). | CodeQL for Rust and Actions (`.github/workflows/codeql.yml`); eleven cargo-fuzz targets (including `mesh_frame` and `mesh_envelope`, be6af99d) under ClusterFuzzLite on PRs and nightly (`fuzz/`, `.github/workflows/cflite_pr.yml`, `cflite_batch.yml`, [docs/security/fuzzing.md](docs/security/fuzzing.md)); the compile-fail suite for the type-enforced rules (`tests/security_compile_fail`, 17 cases, 8c317aa8, [docs/security/compile-fail.md](docs/security/compile-fail.md)). Miri and loom are later E11.4 parts (0.12 → 0.18). | R4 | CodeQL green; fuzz targets with no open crash | closed for SAST and fuzzing (572ec2bd, b7809387); Miri/loom deferred |
| R | A contribution of unknown origin; unsigned commits and tags (S6). | DCO sign-off enforced on every pull request (`.github/workflows/dco.yml`, step `s6_dco_check_rejects_unsigned_off_commit` on a fixture); the publish workflow refuses a lightweight or unverified release tag (`verify-versions`, step `s6_release_requires_signed_tag`); "require signed commits" on protected branches is an owner action ([CONTRIBUTING.md](CONTRIBUTING.md), "Signed commits and tags"). | — | `s6_dco_check_rejects_unsigned_off_commit`; `s6_release_requires_signed_tag` | DCO closed (d6c22982); signed tags closed (edd5e5b8); signed commits: owner action D15, open (2026-10-19: no signature rule on `main`, no ruleset on `release/*`) |
| I | Users do not know which versions get fixes; no threat model (S7). | [SECURITY.md](SECURITY.md) lists 0.11.x and 0.10.x and the security-release history; this file. | — | review at each release (Maintenance) | closed (d6c22982) |
| T | A vulnerable or unlicensed dependency. | `cargo deny check` on every PR and push (`deny.toml`, ci.yml `deny` job) and a daily `cargo audit` (`.github/workflows/audit.yml`); every ignore carries a written reason. | — | `cargo deny check` | mitigated; see L6 and the debts below |

The other `deny.toml` ignores (`deny.toml:19-24`) are unmaintained or unsound notices, not
vulnerabilities in reachable code: `im` 15.1.0 (RUSTSEC-2023-0126 unsound `OrdSet` aliasing,
RUSTSEC-2026-0248 unmaintained) with `bitmaps` and `sized-chunks` through it, used by
`id_effect_fp` and `id_effect_optics`; `atomic-polyfill` via `heapless` 0.7 ← `postcard` ← `foca` /
`id_effect_node`; `unsafe-libyaml` via `serde_yaml` ← `figment` ← `id_effect_config`. Each is open
debt until the dependency is replaced.

## Accepted risks

### L6 — rsa 0.9 (RUSTSEC-2023-0071, Marvin attack)

- **What:** `rsa` 0.9.10 has a timing side channel in RSA **private-key** operations (decryption
  and signing). No fixed release exists upstream.
- **Where it comes from:** `cargo tree --all-features -i rsa -e normal` shows two parents only:
  `openidconnect` 4.0.1 (`id_effect_auth` feature `oidc`, `crates/id_effect_auth/Cargo.toml:104`)
  and `sqlx-mysql` 0.8.6 (`id_effect_sql` feature `mysql`, `crates/id_effect_sql/Cargo.toml:43`).
- **Why it is not reachable:** the workspace performs no RSA private-key operation. `sqlx-mysql`
  uses `rsa` only to encrypt a password with the server's **public** key (`caching_sha2_password` /
  `sha256_password` over a non-TLS connection). `openidconnect` uses it only to verify ID-token
  signatures with the provider's **public** key; no workspace code constructs an RSA private
  signing key (`git grep -nE "RsaPrivate|PrivateSigningKey|rsa::" -- crates` finds only
  `aws_lc_rs::rsa` in a test helper, `crates/id_effect_auth/tests/common/mod.rs:6`). ID tokens the
  workspace verifies itself go through `jsonwebtoken` on `aws-lc-rs`. The attack needs a
  private-key oracle, which neither path exposes.
- **Recorded:** `deny.toml:18` (ignore with this reason) and `.cargo/audit.toml:7`; `cargo deny
  check` stays the gate, so any **new** advisory against `rsa`, or a new dependency path, fails CI.
- **Re-check each release** (owner of the release checklist): re-run the `cargo tree` and `git
  grep` above and confirm the same two parents and no private-key use; check RustSec for a fixed
  `rsa` release, and when one exists, upgrade and remove both ignores. Last re-check: the 0.11.0
  exit sweep (2026-10-19, `release/0.11.0` at 6c0053aa: `rsa` 0.9.10; parents `openidconnect`
  4.0.1 and `sqlx-mysql` 0.8.6; the `git grep` finds only the test helper; no private-key use).

### Trusted cluster (ADR 0009)

A member that holds the mesh cookie (and, with mTLS, a certificate the cluster trusts) is trusted
(B4, last row). Defending against a malicious member is a non-goal of ADR 0009. Re-checked with
ADR 0009 at each release; the R8 work (mTLS on QUIC and TCP, b92af181, ae51878e) narrows who can
become a member but does not change this assumption.

## Residual risks

Not findings: what the closed findings leave to the application or the operator, recorded so that
nobody reads "closed" as "nothing left to do".

- **SSRF behind an HTTP proxy (M3).** Proxying happens below the SSRF connector layer: a
  literal-IP destination is still refused before the proxy is contacted and a named proxy is
  checked by the resolver, but a host **name** is resolved by the proxy, unchecked, and a
  literal-IP proxy is used as given. `ReqwestClient(pub Client)` and `PooledClient::new(client)`
  wrap a client the caller built, unguarded (documented, not recorded: a tuple constructor cannot
  carry `#[deprecated]`). Check foreign URLs with `SsrfPolicy::check_url` when either applies.
- **Plain axum routes (H8).** `AccessLayer` covers the routes of the `Router` that installs it.
  Plain axum routes merged after `Router::into_axum` are outside it unless the application applies
  `Router::access_layer()` (`crates/id_effect_axum/src/route/mod.rs:772`) or an `AccessLayer` over
  its typed and plain routes itself. The service template does (`AccessLayer` over
  `plain_routes()`, `crates/id_effect_tool/templates/service/src/lib.rs.template:237`).
- **Default login throttle store.** `LoginThrottle::default()` counts in a
  `MemoryThrottleStore`, which, unlike `MemoryTotpStepStore`, does not record itself, so the prod
  posture check does not flag a per-process throttle. A service with more than one instance passes
  `LoginThrottle::new(PgThrottleStore::new(..))`.
- **At-least-once handlers.** Reliable mesh delivery deduplicates per sender incarnation only: a
  sender that restarts forgets what it sent, so handlers must be idempotent where a message can
  arrive twice; delivery is to the receiving node, and a message whose target is gone or whose
  mailbox is full is dropped (counted) and still acknowledged (ADR 0009, Amendment 1).
- **Owner actions (D15).** Trusted publisher on every crate, deletion of `CARGO_REGISTRY_TOKEN`
  after the first trusted publish (S3), a registered tag-signing key and "require signed commits"
  on protected branches (S6) are repository settings the owner does; status in
  [docs/RELEASING.md](docs/RELEASING.md) ("Owner steps") and [CONTRIBUTING.md](CONTRIBUTING.md).

## Findings register

Every finding of the 2026-10-18 review. "Closed" names the commit on the release line; the
regression tests are in the boundary tables above.

| ID | Boundary | Rule | Status | Closed by / owner |
|---|---|---|---|---|
| H1 | B1 | R4 | closed | v0.10.1 (r101-http) |
| H2 | B1 | R4 | closed | v0.10.1 (r101-http) |
| H3 | B4 | R8 | closed | v0.10.1 (r101-mesh); opt-out recorded b92af181 |
| H4 | B4 | R8 | closed | v0.10.1 (r101-mesh) |
| H5 | B2 | R4 | closed | v0.10.1 (r101-auth, r101-http); opt-out recorded c2ff95fa; shared store 7af82ba1 |
| H6 | B2 | R10 | closed | v0.10.1 (r101-auth); opt-out recorded c2ff95fa |
| H7 | B2 | R10 | closed | v0.10.1 (r101-auth); opt-outs recorded c2ff95fa |
| H8 | B3 | R1 | closed | cb54bdae (r11-sec-h8-routing); template 888ac28f (r11-w3-kit-template) |
| M1 | B1 | R4 | closed | v0.10.1 (r101-http); opt-outs recorded 38042a63 |
| M2 | B7 | R6 | closed | v0.10.1 (r101-client) |
| M3 | B7 | R6 | closed | 080a9acc (r11-sec-client-ssrf), 9ca8ce57, 7d217442 |
| M4 | B1 | R4 | closed | 5d804c5d (r11-sec-csrf-headers), d13d4d94 (r11-sec-ops-surfaces, template) |
| M5 | B1 | R4 | closed | 8141e0f9 (r11-sec-limits) |
| M6 | B5 | R7 | closed | d13d4d94 (r11-sec-ops-surfaces; Ledger `base.toml` included) |
| M7 | B5 | R7 | closed | d13d4d94 (r11-sec-ops-surfaces) |
| M8 | B1 | R4 | closed | 8141e0f9 (r11-sec-limits) |
| M9 | B4 | R4 | closed | v0.10.1 (r101-mesh) |
| M10 | B4 | R4 | closed | 052d969e (r11-e8-1-mailbox) |
| M11 | B6 | R3 | closed | 72257fb6 (r11-sec-logging); opt-outs recorded ec818604, 6c0053aa |
| M12 | B6 | R3 | closed | 72257fb6 (r11-sec-logging) |
| M13 | B2, B6 | R2 | closed | 745c80dd (r11-sec-secrets) |
| M14 | B2 | R2 | closed | 745c80dd (r11-sec-secrets) |
| M15 | B2, B7 | R6 | closed | v0.10.1 (r101-auth); opt-out renamed and recorded 7d217442 |
| M16 | B6 | R5 | closed | 055e68f0 (r11-sec-sql-redis), lint 1f909018 (r11-e1-9-dylint) |
| M17 | B6 | R5 | closed | 055e68f0 (r11-sec-sql-redis) |
| M18 | B2 | R10 | closed | 7af82ba1 (r11-sec-auth-state) |
| M19 | B3 | R1 | closed | 0aa4186b (r11-sec-tenancy) |
| M20 | B2 | R4 | closed | v0.10.1 (r101-auth) |
| L1 | B1 | R9 | closed | 5d804c5d (r11-sec-csrf-headers) |
| L2 | B1, B5 | R9, R7 | closed | 5d804c5d (headers), d13d4d94 (metrics) |
| L3 | B6 | R2 | closed | 745c80dd (r11-sec-secrets) |
| L4 | B2 | R4 | closed | v0.10.1 (r101-auth) |
| L5 | B1 | R9 | closed | 5d804c5d (API), 7af82ba1 (rotation at login) |
| L6 | B8 | — | **accepted** | see "Accepted risks" (re-checked 2026-10-19) |
| S1 | B8 | R10 | closed | a6a97637 (r11-sc-pin-permissions), edd5e5b8 (`publish.yml`) |
| S2 | B8 | R10 | closed | a6a97637 (r11-sc-pin-permissions) |
| S3 | B8 | — | closed in the workflow; owner steps open | edd5e5b8 (r11-sc-publish); owner action D15 |
| S4 | B8 | R4 | closed (SAST, fuzzing) | 572ec2bd (r11-sc-sast-scorecard), b7809387 (r11-fuzz-ci); mesh targets be6af99d; Miri/loom later E11.4 |
| S5 | B8 | — | closed | a6a97637 (r11-sc-pin-permissions), edd5e5b8 (r11-sc-publish) |
| S6 | B8 | — | closed except signed commits | DCO d6c22982 (r11-sc-dco-docs); signed tag edd5e5b8 (r11-sc-publish); signed commits: owner action D15 |
| S7 | B8 | — | closed | d6c22982 (r11-sc-dco-docs) |

Rules without a finding row of their own are still enforced. R8 covers the mesh mTLS work
(b92af181 QUIC, ae51878e TLS TCP). R10 covers every `dangerous_*` / `insecure_*` opt-out:
`scripts/check-dangerous.sh` (888ac28f, run in `.github/workflows/lint.yml`) fails when such a
function under `crates/` (or a deprecated `*_dangerous` shim) does not call `record_dangerous`,
directly or by delegation; the 0.10.1 opt-outs that did not record now do (38042a63
`ConnectionLimits`, c2ff95fa auth throttle, lockout and JWT, ec818604 CSRF and redaction); and the
prod posture check (`id_effect_kit::posture`, nine named probes) refuses to start a prod profile
with a failed probe or an opt-out not listed under `security.accepted_risks` with a reason
(`posture_prod_fails_on_unaccepted_opt_out`, `crates/id_effect_kit/tests/posture.rs:56`). The
type-enforced rules (R1, R2, R3, R5, R8, R10) have compile-fail cases in
`tests/security_compile_fail` ([docs/security/compile-fail.md](docs/security/compile-fail.md)).

## Maintenance

At each minor release, and at a patch release that fixes a finding:

1. Update the status column of every boundary table and the findings register from the release's
   commits; a closed finding names its commit and regression test.
2. Re-check every accepted risk as its section says and record the date.
3. Add new findings with an id, a boundary, a rule (R1–R10, or "—" for process findings) and an
   owner.
4. Update the supported versions and the "Security releases" history in [SECURITY.md](SECURITY.md).

The 0.11.0 exit criterion: every finding above is closed or recorded here as accepted with a
reason, and 0 highs are open. Status at the exit sweep (2026-10-19): 0 highs open, no finding open
in code; L6 accepted; the D15 owner steps of S3 and S6 (repository and crates.io settings) open.
