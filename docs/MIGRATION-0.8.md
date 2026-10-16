# Migrating to id_effect 0.8

This guide is the "if you used X, now do Y" companion to the `0.8.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.7. If you are on 0.6, apply
[MIGRATION-0.7.md](MIGRATION-0.7.md) first.

Every intentional public-API break in 0.8 is listed here. A break reported by
`cargo semver-checks` (the `semver` CI job, baseline 0.7.0) that is not listed below is a bug.
The deprecation rule and semver policy are in [STABILITY.md](STABILITY.md).

Each 0.8 change appends its entry to the matching section below.

## Web security

`id_effect_axum`'s `server/security.rs` became `server/security/{csrf,cors,csp,headers,cookies}`;
the 0.7 import paths still work.

- `CsrfConfig::new(token)` + `csrf_middleware` (one static token shared by all clients) is
  deprecated. Replace both with `router.layer(SecurityLayer::recommended(key))`: it issues a
  random per-session token in a signed `__Host-csrf` cookie and checks it on every
  non-GET/HEAD/OPTIONS/TRACE request. Clients send it back in `X-CSRF-Token` or a `csrf_token`
  form field. Handlers get it from the `CsrfToken` extractor (returning it sets the header),
  which replaces `set_csrf_header`. Webhooks and other cross-site endpoints need
  `.csrf_exempt("/webhooks")`; cookie-less APIs can use `.without_csrf()`. The deprecated
  `csrf_middleware` now compares in constant time.
- `csp_middleware` is deprecated: `SecurityLayer` applies `ContentSecurityPolicy::recommended()`
  (nonce-based) or your `.csp(policy)?`. Inline scripts need `nonce="{CspNonce}"` or
  `.script_hash(content)`. A handler's own CSP header is no longer overwritten.
  `ContentSecurityPolicy::to_header_value()` returns `None` for a policy that fails `validate()`
  (a directive value containing `;`, `,`, whitespace or control characters).
- Provide the cookie key as an effect layer: `CookieKey::layer::<E>()` reads `COOKIE_KEY`
  (base64, at least 64 random bytes) from config; `CookieKey::layer_from(key)` wraps a key you
  already have.
- CORS: `CorsConfig::new().allow_origin(..).allow_credentials(true).build()?` returns a
  `tower_http` `CorsLayer`; credentials with a wildcard origin is
  `SecurityError::CorsCredentialsWithWildcard`.
- The deprecated items are removed in 0.9.

## CLI

- `id_effect_cli::RunMainConfig` is `#[non_exhaustive]` and has a new field
  `interrupt_on_ctrl_c` (default `true`). Struct literals such as
  `RunMainConfig { install_tracing: true }` no longer compile; use
  `RunMainConfig::with_tracing()` or `RunMainConfig::minimal().tracing(true)`.
- Behaviour: under `run_main`, Ctrl-C no longer kills the process at once. It interrupts the
  program so `ensuring` / `acquire_use_release` finalizers run, then exits 130 with
  `error: interrupted (…)` on stderr; a second Ctrl-C exits 130 immediately. To keep the old
  handling (or install your own), use `.ctrl_c(false)`.

## Streaming

- `Sink::fold_left`, `Sink::from_fold`, `Sink::collect`, `Sink::drain` and
  `Sink::collect_to_map` return `FoldSink<A, In, E, R>` instead of `Sink`. `zip` is a method of
  `FoldSink`, and `FoldChannel::from_sink` takes a `FoldSink`. Passing any other sink
  (`collect_all_while`, `for_each`, `to_queue`, `Sink::new`) used to panic at run time and is
  now a type error. `sink.run(stream)` and `stream.run(sink)` work unchanged (`Stream::run` takes
  `impl Into<Sink>`). Where a plain `Sink` is required (a field, a return type, a
  `Vec<Sink<…>>`), convert with `.into()` or `Sink::from(fold_sink)`. `Sink`'s `Debug` output no
  longer has a `fold` field.

## Testing

- The 0.6 test helpers removed in 0.7 return in a 0.8 form. `run_test(eff)` (0.6 took
  `(eff, env)`; provide `R` first) runs on a `TestExecutor` and skips through virtual time.
  `assert_exit_success` / `assert_exit_eq` keep their 0.6 signatures; new are
  `assert_exit_fail`, `assert_exit_die`, `assert_exit_interrupted`, `assert_exit_kind` and
  `expect_exit_*`. 0.6 `SnapshotAssertion` becomes file-based `assert_golden(path, text)`,
  `assert_golden_debug` and `assert_golden_exit` (`ID_EFFECT_BLESS=1` rewrites the files).
- The `proptest` feature returns: `id_effect::testing::property` (0.6 `testing::proptest`) keeps
  `arb_exit_success` / `arb_exit_fail` / `prop_assert_exit_eq` / `prop_assert_exit_success` and
  adds schema, `Cause`, `Duration` and `Chunk` generators, `prop_check` and `StateMachine`.
  0.6 `run_effect` / `success_value` become `prop_run` / `prop_check`.

## Observability

- `id_effect_axum::Host` has a new public field `readiness: ReadinessState`;
  `HostBuilder::build` sets it ready. Code that builds a `Host` with a struct literal must add
  `readiness: ReadinessState::new(true)`.
- On SIGINT/SIGTERM, `HostBuilder::run_until_shutdown` keeps the `serve` future running while it
  drains (it used to drop it first). `serve_router` and `ServerHandle::shutdown` mark readiness
  shutting down before they drain or stop, so `/ready` answers 503. `ReadinessState::set_ready(true)`
  also clears a shutting-down mark.
