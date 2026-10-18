# Migrating to id_effect 0.11

This guide is the "if you used X, now do Y" companion to the `0.11.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.10. If you are on 0.9, apply
[MIGRATION-0.10.md](MIGRATION-0.10.md) first.

Every intentional public-API break in 0.11 is listed here. A break reported by
`cargo semver-checks` (the `semver` CI job, baseline the last 0.10.x release) that is not listed
below is a bug. The deprecation rule and semver policy are in [STABILITY.md](STABILITY.md).

Each 0.11 change fills the matching section below.

## Routing: every route is public or guarded (H8)

Filled by `r11-sec-h8-routing`.

## SQL: `Sql` and `RawSql`

Filled by `r11-sec-sql-redis`.

## Processes: `Next`, `Info` and `SendError`

Filled by `r11-e8-1-mailbox` (`SendError`) and `r11-e8-1-timers-hibernate` (`Next`, `Info`).

## Logging: `RedactionPolicy::new`

Filled by `r11-sec-logging`.

## Config: `Secret<T: Zeroize>` and its equality

Filled by `r11-sec-secrets`.

## STM: `id_effect_fp::stm` moves to `id_effect::stm`

Filled by `r11-e2-5-stm-core`.

## HTTP client: SSRF guard on by default

Filled by `r11-sec-client-ssrf`.

## Node: plaintext TCP needs the `insecure-plaintext-tcp` feature

Filled by `r11-e8-3-tcp-tls`.

## Web: CSRF masked token format

Filled by `r11-sec-csrf-headers`.

## Node: mesh protocol version

Filled by `r11-e8-3-reliable-delivery`.
