# Threat model

This is the threat model of the published id_effect crates (`crates/*`) and the way they are built
and released. It lists, per trust boundary, what an attacker can try (STRIDE), what stops it, which
rule of [ADR 0013 "Secure by default"](docs/adrs/0013-secure-by-default.md) (R1–R10) the mitigation
belongs to, and the test or CI gate that keeps it true. It also tracks every finding of the
2026-10-18 security review: closed, open (with the 0.11 leaf that owns it) or accepted (with the
reason).

Status is as of `release/0.11.0` at 0f615124 (0.10.1 merged). Each release re-checks every row
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
| S | A client spoofs its IP with `X-Forwarded-For` to dodge per-IP limits (H1, H2). | `ClientIp` reads `X-Forwarded-For` only when the socket peer is a `TrustedProxies` entry, from the right (`crates/id_effect_axum/src/client_ip.rs:90`); every serve path records `ConnectInfo` through `serve_listener` (`server/connection.rs:167`). | R4 | `h1_per_ip_limit_sees_the_peer`, `h1_forwarded_for_behind_a_trusted_proxy_keys_the_client` (`crates/id_effect_axum/tests/connection.rs:112`, `:136`); `h2_ledger_ignores_spoofed_forwarded_for` (`examples/reference/ledger/tests/authn.rs:319`) | closed (v0.10.1) |
| T | Cross-site request forgery on cookie-authenticated forms. | `SecurityLayer` CSRF check (`server/security/mod.rs:139`, `server/security/csrf.rs`). | R9 | `crates/id_effect_axum/tests/security_defaults.rs` | mitigated; token hardening open (L1, L5) |
| T, I | BREACH recovers the static CSRF token from compressed responses (L1); the token is not rotated at login (L5). | Masked token per response and a rotation API (r11-sec-csrf-headers); rotation at login (r11-sec-auth-state). | R9 | `l1_csrf_token_masked_differently_per_response`, `l5_rotate_issues_new_token_and_invalidates_old`, `l5_csrf_token_rotates_at_login` | open: r11-sec-csrf-headers, r11-sec-auth-state |
| I | Missing COOP, CORP and Permissions-Policy headers (L2, headers part). | Recommended headers in `server/security/headers.rs`. | R9 | `l2_recommended_headers_include_coop_corp_permissions_policy` | open: r11-sec-csrf-headers |
| R | A request cannot be traced to a log line. | Request ids (`middleware/request_id.rs`); escaped log lines (B6). | R3 | `m12_*` (B6) | mitigated |
| D | Slowloris, connection floods, unbounded keep-alive (M1). | `ConnectionLimits` (`server/connection.rs:52`): 10 s header-read timeout, 10,000 connections, 1,000 requests per connection; opt-outs `dangerous_no_header_read_timeout`, `dangerous_unlimited_connections`, `dangerous_unlimited_requests_per_connection`. | R4, R10 | `m1_slow_headers_are_cut_off`, `m1_connection_cap_holds`, `m1_keep_alive_limit_closes_the_connection` (`tests/connection.rs:167`, `:213`, `:244`) | closed (v0.10.1) |
| D | Oversized bodies. | `BodyLimitLayer` (`middleware/limits.rs:138`). | R4 | `crates/id_effect_axum/tests/limits.rs` | mitigated |
| D | The template CSRF layer buffers a 2 MiB form outside the limit and timeout layers (M4). | `FORM_LIMIT` (`server/security/csrf.rs:40`, read at `:225`) bounded by the configured limit and timeout; template order puts the limit layers outside `SecurityLayer`. | R4 | `m4_csrf_form_read_respects_limit`, `m4_csrf_form_read_times_out`, `m4_template_limit_layers_wrap_security_layer` | open: r11-sec-csrf-headers (layer), r11-sec-ops-surfaces (template) |
| D | No concurrency cap or load shedding (M5). | Load-shed layer in the default server stack. | R4 | `m5_load_shed_rejects_over_capacity_with_503`, `m5_default_server_stack_has_concurrency_cap`; posture probe "load shedding on" | open: r11-sec-limits |
| D | Rate-limit eviction scans every bucket under one mutex (M8). | Today: `MemoryRateLimitStore` evicts with `min_by_key` over all buckets at the `max_keys` cap (`middleware/rate_limit.rs:141-144`). Planned: sharded LRU store. | R4 | `m8_eviction_does_not_scan_all_keys`, `m8_eviction_evicts_least_recently_used`, `m8_sharded_store_under_contention` | open: r11-sec-limits |
| E | A route without a guard serves anonymous callers (H8). | See B3. | R1 | B3 | open: r11-sec-h8-routing |

## B2 — Authentication

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| S | Online password guessing (H5). | `LoginThrottle` (`crates/id_effect_auth/src/throttle.rs:167`): 5 free failures per account and per client IP, then a doubling delay up to 15 min, before the user lookup and before hashing; opt-out `BasicAuth::without_throttle_dangerous`. | R4, R10 | `h5_throttle_checked_before_hashing` (`src/basic.rs:437`), `h5_login_throttle_backs_off` (`tests/credentials.rs:311`), `h5_ledger_login_is_throttled` (`examples/reference/ledger/tests/authn.rs:275`) | closed (v0.10.1) |
| S | TOTP brute force (H6). | Lockout after 5 wrong codes; opt-out `without_lockout_dangerous` (`src/totp.rs:402`). | R10 | `h6_totp_locks_after_five_failures` (`src/totp.rs:860`) | closed (v0.10.1) |
| S | A TOTP code replayed on a second instance (M18): the step store is memory-only (`MemoryTotpStepStore`, `src/totp.rs:273`). | Postgres step store shared by all instances. | R10 | `m18_pg_totp_replay_rejected_across_instances`, `m18_pg_totp_concurrent_consume_one_wins` | open: r11-sec-auth-state |
| S | A JWT minted for another issuer or audience is accepted (H7). | A `JwtVerifier` with neither issuer nor audience rejects every token (`AuthError::Config`); opt-outs `any_issuer_dangerous` (`src/jwt.rs:482`) and `any_audience_dangerous`. | R10 | `h7_jwt_without_issuer_or_audience_is_rejected`, `h7_dangerous_opt_out_is_explicit` (`src/jwt.rs:1162`, `:1181`) | closed (v0.10.1) |
| S, T | Forged tokens through a weak HMAC key or a JWKS served over plain http or a redirect (M15). | HMAC secrets under 32 bytes fail; JWKS over https only (loopback excepted), no redirects, 1 MiB cap; opt-out `allow_http_dangerous`. | R6, R10 | `m15_short_hmac_key_rejected` (`src/jwt.rs:1199`), `m15_jwks_requires_https` (`src/jwks.rs:360`), `m15_jwks_body_capped`, `m15_jwks_redirects_and_plain_http_are_refused` (`tests/jwks.rs:183`, `:207`) | closed (v0.10.1) |
| I | `Session`'s derived `Debug` prints its attributes, including OIDC tokens (M14; `src/session/mod.rs:106`). | Redacting `Debug`. | R2 | `m14_session_debug_hides_attr_values_and_tokens` | open: r11-sec-secrets |
| I | Passwords stay in memory after use (M13; `Password(String)`, `src/password.rs:122`). | Zeroize on drop. | R2 | `m13_password_is_zeroized_on_drop` | open: r11-sec-secrets |
| D | Hashing as a CPU-exhaustion vector (M20, L4). | Bounded hashing pool (2 × parallelism, 5 s wait, `PasswordError::Busy` → 503); `BasicAuth` rejects passwords over 256 bytes before hashing. | R4 | `m20_hashing_is_bounded` (`src/password.rs:750`), `l4_long_basic_password_rejected` (`src/basic.rs:497`) | closed (v0.10.1) |
| R | Failed logins leave no trace. | Auth failures are typed (`AuthError`, e.g. `TooManyAttempts` → 429) and answered with a status code; the application logs them, with the request id, through the redacting and escaping logger (R3). There is no built-in authentication audit log. | R3 | `h5_*`, `m11_*`, `m12_*` | partly mitigated; audit log not planned for 0.11 |
| E | The login throttle is per process, so N instances allow N × the budget. | The 0.10.1 throttle trait gets a Postgres store. | R10 | r11-sec-auth-state tests (`auth_state.rs`) | open: r11-sec-auth-state |

## B3 — Authorization

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| E | Routes are open unless guarded; an unauthenticated request continues as `Anonymous` (`crates/id_effect_auth/src/middleware.rs:10`, `crates/id_effect_authz/src/guard.rs:13`) and `Router::route` accepts any endpoint (`crates/id_effect_axum/src/route/mod.rs:580`) (H8). | Typestate routes: `.public()` or `.guard(g)` before `Router::route`; `route_unchecked` as a deprecated, recorded shim; deny-by-default layer for unregistered axum routes (0.11.0 D8). | R1, R10 | `h8_route_without_access_decision_does_not_compile`, `h8_route_unchecked_is_deprecated_and_recorded`, `h8_unregistered_axum_route_is_denied`, `h8_anonymous_reaches_only_public_routes`, `h8_access_report_lists_public_routes` | open: r11-sec-h8-routing (the one open high) |
| E, I | Cross-tenant access when a policy forgets the tenant check (M19). | A tenanted resource requires the tenant match by default (`same_tenant`, `crates/id_effect_authz/src/resource.rs:130`); crossing tenants needs `dangerous_cross_tenant(reason)` (`src/policy.rs:266`). | R1, R10 | `m19_tenanted_resource_requires_tenant_match_by_default`, `m19_cross_tenant_requires_named_opt_out`, `m19_untenanted_resource_unaffected` (`crates/id_effect_authz/tests/tenancy.rs:71`, `:163`, `:211`) | closed (0aa4186b) |
| R | Nobody can list which routes are public. | Access report of public and guarded routes. | R1 | `h8_access_report_lists_public_routes`; Ledger `ledger_access_report_snapshot` | open: r11-sec-h8-routing, r11-w3-ledger |

## B4 — Mesh

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| S | A man in the middle poses as a QUIC server (H3). | Dialing needs trusted roots, a pinned leaf SHA-256 (`QuicConfig::with_pinned_server_cert_sha256`, `crates/id_effect_node/src/transport/quic.rs:115`) or `dangerous_accept_any_server_cert` (`:130`). | R8, R10 | `h3_quic_without_roots_is_a_config_error`, `h3_dangerous_accept_any_is_explicit` (`crates/id_effect_node/tests/mesh_security.rs:81`, `:109`) | closed (v0.10.1) |
| S, T | A relayed, reflected or replayed handshake proof (H4). | The proof covers version, role, both node names and incarnations, both nonces and, on QUIC, a TLS exporter secret; protocol version 2 (`src/wire.rs:21`). | R8 | `h4_relayed_proof_fails`, `h4_relayed_proof_fails_without_a_channel`, `h4_v1_peers_are_refused_with_version_mismatch` (`tests/mesh_security.rs:145`, `:256`, `:419`) | closed (v0.10.1) |
| S | A peer without a client certificate joins (servers use `with_no_client_auth()`, `transport/quic.rs:435-455`); QUIC is not mutually authenticated beyond the cookie. | mTLS on QUIC and a TLS-wrapped TCP transport; plaintext TCP behind a feature and a named opt-out. | R8 | `mtls_rejects_peer_without_client_cert`; compile-fail "plaintext TCP without the feature"; posture probe "mesh transport encrypted" | open: r11-e8-3-quic-mtls, r11-e8-3-tcp-tls |
| I, T | The TCP transport is plaintext; it warns once (`transport/tcp.rs:32`). | As above (R8). | R8 | as above | open: r11-e8-3-tcp-tls |
| D | Pre-authentication resource use: slow handshakes, large frames, short cookies, small nonces (M9). | 5 s handshake timeout (`src/handshake.rs:119`), 4 KiB frames until authenticated (`PRE_AUTH_MAX_FRAME_BYTES`, `:44`, applied `:282`), cookies of at least 32 bytes (opt-out `Cookie::dangerous_allow_short`), 128-bit nonces. | R4, R10 | `m9_handshake_times_out`, `m9_oversized_pre_auth_frame_rejected`, `m9_frame_cap_lifts_after_authentication`, `m9_short_cookie_rejected` (`tests/mesh_security.rs:316-380`), `m9_nonces_are_128_bit` (`src/handshake.rs:900`) | closed (v0.10.1) |
| D | Unbounded process mailboxes reachable from remote sends (M10). | Every mailbox bounded (default `DEFAULT_MAILBOX_CAPACITY` 10,000, `crates/id_effect_process/src/mailbox.rs:48`); local sends fail with `SendError::Full`; remote sends to a full mailbox fault; system messages bypass the bound. | R4 | `m10_bounded_mailbox_rejects_when_full`, `m10_system_messages_bypass_bound` (`crates/id_effect_process/tests/mailbox_bounds.rs:86`, `:142`), `m10_remote_send_to_full_mailbox_faults` (`crates/id_effect_node/src/registry.rs:490`) | closed (052d969e) |
| T | Malformed payloads from a peer. | Payloads are decoded through `id_effect::schema` at the wire boundary (ADR 0009, "Wire protocol"); fuzz targets cover the schema decoder (`fuzz/fuzz_targets/schema_json.rs`). | R4 | ClusterFuzzLite (`.github/workflows/cflite_pr.yml`) | mitigated |
| E | A cookie holder spawns registered behaviours or sends to any process. | By design (ADR 0009 trusted-cluster assumption): only behaviours registered in the binary can be spawned, with Schema-validated arguments; no code mobility. | — | accepted assumption (ADR 0009 non-goals) | accepted (design) |

## B5 — Console and operator surfaces

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| E | The console API is reachable without a guard. | `Console::router(guard)` is the only way to get its routes (`crates/id_effect_console/src/api.rs:141`). | R7 | `crates/id_effect_console/tests/{api,console,csrf}.rs` | mitigated |
| S, E | The template console treats every loopback request as an operator and checks neither Host nor Origin (DNS rebinding); the Ledger enables the console in every profile (M6; `crates/id_effect_tool/templates/service/src/console.rs.template:73`, `examples/reference/ledger/config/base.toml:78`). | Loopback operator in the dev profile only; Host/Origin check; Ledger console off in prod (0.11.0 D11). | R7 | `m6_console_rejects_foreign_host_and_origin`, `m6_loopback_operator_only_in_dev`, `m6_ledger_console_disabled_in_prod_profile`; posture probe "console guarded" | open: r11-sec-ops-surfaces, r11-w3-ledger |
| T | `/docs` loads the Scalar bundle from jsDelivr at `@1` without Subresource Integrity (M7; `crates/id_effect_axum/src/openapi/ui.rs:69`, `:103`). | Exact version plus SRI hash by default; `/docs` closed in prod unless guarded. | R7 | `m7_scalar_bundle_pinned_with_integrity`, `m7_docs_disabled_in_prod_unless_guarded`; posture probe "docs closed" | open: r11-sec-ops-surfaces |
| I | The template serves `/metrics` on the public router (L2, metrics part; `templates/service/src/lib.rs.template:131`). | `/metrics` on the console listener or behind a guard. | R7 | `l2_template_metrics_not_public` | open: r11-sec-ops-surfaces |
| R | An operator's runtime change (log level, job retry or cancel) is not attributable. | Console actions need the guard's principal; they are not written to an audit log. | R7 | `crates/id_effect_console/tests/api.rs` | partly mitigated; audit log not planned for 0.11 |

## B6 — Data: SQL, Redis, secrets and logs

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| T, E | SQL injection through a formatted query string (M16). | `query`/`execute` take `impl Into<Sql>` where `Sql: From<&'static str>`; raw SQL needs `RawSqlCapability`, provided only by `RawSqlCapability::layer_dangerous()` (`crates/id_effect_sql/src/raw.rs:125`, `:135`); Dylint lint `raw_sql_non_literal` on the `SqlClient` trait methods (`lints/id_effect_lints/src/lib.rs:477`). | R5, R10 | `m16_query_with_string_does_not_compile`, `m16_query_raw_requires_capability` (`crates/id_effect_sql/tests/ui.rs:27`, `:35`), `m16_literal_query_still_compiles` (`src/raw.rs:180`); `.github/workflows/lint.yml` | closed (055e68f0, 1f909018) |
| T, D | Redis `clear()` with an empty namespace deletes every key (M17; `namespace_glob`, `crates/id_effect_redis/src/client.rs:356`). | An empty namespace is an error; `clear` deletes only namespace keys. | R5 | `m17_clear_with_empty_namespace_is_an_error`, `m17_clear_only_deletes_namespace_keys` (`crates/id_effect_redis/tests/store_clear.rs:15`, `:42`) | closed (055e68f0) |
| I | Secrets are never zeroized (M13; `Secret<T>`, `crates/id_effect_config/src/secret.rs:16`). | `Secret<T: Zeroize>` zeroized on drop. | R2 | `m13_secret_is_zeroized_on_drop` | open: r11-sec-secrets |
| I | `Secret` equality is not constant time (L3; `secret.rs:56`). | Constant-time comparison; compile-fail on generic `Secret` equality. | R2 | `l3_secret_eq_uses_constant_time_compare`; `tests/security_compile_fail` | open: r11-sec-secrets |
| I | Secrets in log fields (M11): `RedactionPolicy::new()` redacted nothing. | The default pipeline redacts common secret fields; an empty policy must be explicit (`crates/id_effect_logger/src/redact.rs:49`). | R3 | `m11_default_pipeline_redacts_common_secret_fields`, `m11_empty_policy_is_explicit` (`crates/id_effect_logger/tests/security.rs:70`, `:147`) | closed (72257fb6) |
| R, T | Log forging: newlines, control and ANSI sequences, or crafted keys inject lines and fields (M12). | Messages, values and keys are escaped (`crates/id_effect_logger/src/pipeline.rs`). | R3 | `m12_newline_in_message_cannot_forge_a_line`, `m12_annotation_key_cannot_inject_fields`, `m12_control_and_ansi_sequences_are_escaped` (`tests/security.rs:181`, `:203`, `:226`) | closed (72257fb6) |
| T | A stale workflow engine appends to a history another engine owns. | Lease with a fencing token checked on every append (0.11.0 D7, `crates/id_effect_workflow`). | — | `stale_writer_append_is_rejected_by_fencing` (`crates/id_effect_workflow/tests/engine_pg.rs:360`) | mitigated (5d3fc364) |

## B7 — Outbound HTTP

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| E, I | Server-side request forgery to loopback, private ranges or cloud metadata, and redirects followed to them (M3). | SSRF guard on in `HttpClientConfig::default()`: every resolved address checked (DNS-rebinding safe); no redirects by default, re-checked per hop when enabled; allowlist from config; `dangerous_allow_private_networks()` recorded (0.11.0 D10). | R6, R10 | `m3_ssrf_blocks_loopback_private_and_metadata`, `m3_ssrf_checks_every_resolved_address`, `m3_ipv4_mapped_ipv6_is_blocked`, `m3_redirects_not_followed_by_default`, `m3_followed_redirect_to_private_ip_is_blocked`, `m3_allowlist_permits_named_host`; posture probe "SSRF guard on" | open: r11-sec-client-ssrf |
| D | A hostile server streams an unbounded body (M2). | `max_body_bytes` (default 10 MiB, `crates/id_effect_platform/src/http/mod.rs:63`) enforced on a declared `Content-Length` before reading and while streaming. | R6 | `m2_content_length_over_cap_rejected_before_reading`, `m2_streaming_body_over_cap_stops_early`, `m2_body_exactly_at_cap_is_accepted` (`crates/id_effect_platform/tests/http_body_cap.rs:110`, `:148`, `:181`) | closed (v0.10.1) |
| D | A slow server holds a request open. | Client timeouts (0.10, `http/mod.rs`). | R6 | `crates/id_effect_platform` tests | mitigated |
| S | A spoofed JWKS or OIDC endpoint. | https only, no redirects (B2, M15). | R6 | `m15_*` | closed (0.10.1) |

## B8 — Supply chain

| STRIDE | Threat | Mitigation | Rule | Test or gate | Status |
|---|---|---|---|---|---|
| T | A moved action tag or a floating toolchain changes what CI runs; `curl \| bash` installers (S1). | Every `uses:` pinned by full commit SHA with a version comment; pinned toolchains. New workflows already follow it (`codeql.yml`, `scorecard.yml`, `cflite_*.yml`, `dco.yml`). | R10 | `s1_every_action_pinned_by_sha` (`scripts/check-workflows.sh --self-test`) | open: r11-sc-pin-permissions (+ `publish.yml` in r11-sc-publish) |
| E | Workflows run with the default token permissions; Dependabot auto-merge approves its own pull requests (S2). | Top-level `permissions: contents: read`, job-level grants only where needed. | R10 | `s2_every_workflow_least_privilege`, `s2_dependabot_does_not_self_approve` | open: r11-sc-pin-permissions |
| S, E | A leaked long-lived `CARGO_REGISTRY_TOKEN` publishes a malicious crate (S3). | crates.io trusted publishing (OIDC, no stored token); the secret deleted after the first trusted publish (0.11.0 D15, owner action). | — | `s3_no_registry_token_in_workflows` | open: r11-sc-publish, owner action D15 |
| T | Provenance attests crates re-downloaded from crates.io, not the files CI built; `Cargo.lock` is not committed (S5). | `cargo package --locked`, SLSA Build L3 provenance over the built `.crate` files, a verify job comparing the published sha256 with the attested digest (0.11.0 D12, D13). | — | `s5_attested_digest_matches_published_crate` | open: r11-sc-publish, r11-sc-pin-permissions |
| T | Memory-safety and parser bugs nobody looked for (S4). | CodeQL for Rust and Actions (`.github/workflows/codeql.yml`); nine cargo-fuzz targets under ClusterFuzzLite on PRs and nightly (`fuzz/`, `.github/workflows/cflite_pr.yml`, `cflite_batch.yml`). Miri and loom are later E11.4 parts (0.12 → 0.18). | R4 | CodeQL green; fuzz targets with no open crash | closed for SAST and fuzzing (572ec2bd, b7809387); Miri/loom deferred |
| R | A contribution of unknown origin; unsigned commits and tags (S6). | DCO sign-off enforced on every pull request (`.github/workflows/dco.yml`, step `s6_dco_check_rejects_unsigned_off_commit` on a fixture); signed annotated release tags required by the publish workflow; "require signed commits" on protected branches is an owner action ([CONTRIBUTING.md](CONTRIBUTING.md), "Signed commits and tags"). | — | `s6_dco_check_rejects_unsigned_off_commit`; `s6_release_requires_signed_tag` | DCO closed by r11-sc-dco-docs; signed tag: r11-sc-publish; signed commits: owner action D15 |
| I | Users do not know which versions get fixes; no threat model (S7). | [SECURITY.md](SECURITY.md) lists 0.11.x and 0.10.x and the security-release history; this file. | — | review at each release (Maintenance) | closed by r11-sc-dco-docs |
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
  `openidconnect` 4.0.1 (`id_effect_auth` feature `oidc`, `crates/id_effect_auth/Cargo.toml:102`)
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
  `rsa` release, and when one exists, upgrade and remove both ignores. Last re-check: 0.11.0
  (2026-10-19, `rsa` 0.9.10; parents `openidconnect` 4.0.1 and `sqlx-mysql` 0.8.6; no private-key
  use).

### Trusted cluster (ADR 0009)

A member that holds the mesh cookie is trusted (B4, last row). Defending against a malicious
member is a non-goal of ADR 0009. Re-checked with ADR 0009 at each release; the R8 work (mTLS)
narrows who can become a member but does not change this assumption.

## Findings register

Every finding of the 2026-10-18 review. "Closed" names the commit on the release line; the
regression tests are in the boundary tables above.

| ID | Boundary | Rule | Status | Closed by / owner |
|---|---|---|---|---|
| H1 | B1 | R4 | closed | v0.10.1 (r101-http) |
| H2 | B1 | R4 | closed | v0.10.1 (r101-http) |
| H3 | B4 | R8 | closed | v0.10.1 (r101-mesh) |
| H4 | B4 | R8 | closed | v0.10.1 (r101-mesh) |
| H5 | B2 | R4 | closed | v0.10.1 (r101-auth, r101-http) |
| H6 | B2 | R10 | closed | v0.10.1 (r101-auth) |
| H7 | B2 | R10 | closed | v0.10.1 (r101-auth) |
| H8 | B3 | R1 | **open** | r11-sec-h8-routing |
| M1 | B1 | R4 | closed | v0.10.1 (r101-http) |
| M2 | B7 | R6 | closed | v0.10.1 (r101-client) |
| M3 | B7 | R6 | open | r11-sec-client-ssrf |
| M4 | B1 | R4 | open | r11-sec-csrf-headers, r11-sec-ops-surfaces |
| M5 | B1 | R4 | open | r11-sec-limits |
| M6 | B5 | R7 | open | r11-sec-ops-surfaces, r11-w3-ledger |
| M7 | B5 | R7 | open | r11-sec-ops-surfaces |
| M8 | B1 | R4 | open | r11-sec-limits |
| M9 | B4 | R4 | closed | v0.10.1 (r101-mesh) |
| M10 | B4 | R4 | closed | 052d969e (r11-e8-1-mailbox) |
| M11 | B6 | R3 | closed | 72257fb6 (r11-sec-logging) |
| M12 | B6 | R3 | closed | 72257fb6 (r11-sec-logging) |
| M13 | B2, B6 | R2 | open | r11-sec-secrets |
| M14 | B2 | R2 | open | r11-sec-secrets |
| M15 | B2, B7 | R6 | closed | v0.10.1 (r101-auth) |
| M16 | B6 | R5 | closed | 055e68f0 (r11-sec-sql-redis), lint 1f909018 (r11-e1-9-dylint) |
| M17 | B6 | R5 | closed | 055e68f0 (r11-sec-sql-redis) |
| M18 | B2 | R10 | open | r11-sec-auth-state |
| M19 | B3 | R1 | closed | 0aa4186b (r11-sec-tenancy) |
| M20 | B2 | R4 | closed | v0.10.1 (r101-auth) |
| L1 | B1 | R9 | open | r11-sec-csrf-headers |
| L2 | B1, B5 | R9, R7 | open | r11-sec-csrf-headers (headers), r11-sec-ops-surfaces (metrics) |
| L3 | B6 | R2 | open | r11-sec-secrets |
| L4 | B2 | R4 | closed | v0.10.1 (r101-auth) |
| L5 | B1 | R9 | open | r11-sec-csrf-headers (API), r11-sec-auth-state (call site) |
| L6 | B8 | — | **accepted** | see "Accepted risks" (r11-sc-dco-docs) |
| S1 | B8 | R10 | open | r11-sc-pin-permissions, r11-sc-publish |
| S2 | B8 | R10 | open | r11-sc-pin-permissions |
| S3 | B8 | — | open | r11-sc-publish, owner action D15 |
| S4 | B8 | R4 | closed (SAST, fuzzing) | 572ec2bd (r11-sc-sast-scorecard), b7809387 (r11-fuzz-ci); Miri/loom later E11.4 |
| S5 | B8 | — | open | r11-sc-publish, r11-sc-pin-permissions |
| S6 | B8 | — | partly closed | DCO: r11-sc-dco-docs; signed tag: r11-sc-publish; signed commits: owner action D15 |
| S7 | B8 | — | closed | r11-sc-dco-docs |

Rules without a finding row of their own are still enforced: R8 also covers the mesh mTLS work
(r11-e8-3-quic-mtls, r11-e8-3-tcp-tls), and R10 covers every `dangerous_*` / `insecure_*` opt-out
(`scripts/check-dangerous.sh`, r11-w3-kit-template; prod posture check
`posture_prod_fails_on_unaccepted_opt_out`).

## Maintenance

At each minor release, and at a patch release that fixes a finding:

1. Update the status column of every boundary table and the findings register from the release's
   commits; a closed finding names its commit and regression test.
2. Re-check every accepted risk as its section says and record the date.
3. Add new findings with an id, a boundary, a rule (R1–R10, or "—" for process findings) and an
   owner.
4. Update the supported versions and the "Security releases" history in [SECURITY.md](SECURITY.md).

The 0.11.0 exit criterion: every finding above is closed or recorded here as accepted with a
reason, and 0 highs are open.
