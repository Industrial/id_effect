# Migrating to id_effect 0.12

This guide is the "if you used X, now do Y" companion to the `0.12.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.11. If you are on 0.10, apply
[MIGRATION-0.11.md](MIGRATION-0.11.md) first.

Every intentional public-API break in 0.12 is listed here, each with its shim (rule L9) or the
reason it has none. A break reported by `cargo semver-checks` (the `semver` CI job, baseline the
last 0.11.x release) that is not listed below is a bug. The deprecation rule and semver policy are
in [STABILITY.md](STABILITY.md).

0.12 is the "one call surface" release (ADR 0014): RPC is the first-party call surface, REST +
OpenAPI stays the public API style, and HTTP/3 is a standard transport with TCP fallback.

> Status: skeleton (leaf `r12-t0-skeletons`). One heading per planned break; each leaf that ships
> the break fills in its section, and leaf `r12-t0-migration-guide` completes the guide.

## Upgrade checklist

## Node: `transport::certs` moved to `id_effect_platform::tls`

## Axum: TLS and HTTP/3 server configuration fields

## HTTP client: `HttpClientConfig::http3`

## Node: the QUIC mesh requires ALPN `idm/1`

## RPC: labs `RpcGroup::register` becomes `#[rpc::service]`

## RPC: OpenAPI output is 3.1 (was 3.0)

## Templates: `server.http3` in the `service` template

## Semver report: each reported break and its entry
