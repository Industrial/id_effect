# Migrating to id_effect 0.13

This guide is the "if you used X, now do Y" companion to the `0.13.0` section of
[CHANGELOG.md](../CHANGELOG.md). It assumes you are on 0.12. If you are on 0.11, apply
[MIGRATION-0.12.md](MIGRATION-0.12.md) first.

Every intentional public-API break in 0.13 is listed here, each with its shim (rule L9) or the
reason it has none. A break reported by `cargo semver-checks` (the `semver` CI job, baseline the
last 0.12.x release) that is not listed below is a bug. The deprecation rule and semver policy are
in [STABILITY.md](STABILITY.md).

0.13 is the "Breadth" release: AI (`id_effect_ai`, promoted from labs), an admin back office
(`id_effect_admin`), server-rendered UI (`id_effect_axum` feature `ui`), i18n (`id_effect_i18n`),
ORM relations, keyset pagination and lifecycle hooks, and tenancy helpers.

Each section below names the leaf that owns it; the leaf writes its body when it lands, and
`r13-t0-migration-guide` checks the list against `cargo semver-checks` at the release exit.

## SQL: `id_effect_sql::repository` split into a module tree

Owner: r13-e3-3-relations. The paths of 0.12 stay importable through `pub use`.

## SQL: `Entity` gains associated items

Owners: r13-e3-3-relations, r13-e3-3-hooks. Every new item has a default, so manual `Entity`
implementations keep compiling.

## Events: `EventLog::head`

Owner: r13-f-eventlog-head. A provided method; implementors outside the workspace need no change.

## Axum: `WsConfig` control-frame cap

Owner: r13-f-ws-credit.

## Axum / platform: `Http3Options` early-data and congestion settings

Owner: r13-f-h3-settings.

## RPC: `HttpTransport::with_header` / `with_bearer`

Owner: r13-f-client-batching.

## TLS: rcgen 0.14

Owner: r13-f-rcgen. Only a break if an rcgen type appears in a public signature.

## CLI: `crates/id_effect_cli/templates/admin-stub` removed

Owner: r13-t0-skeletons. The `admin-stub` directory shipped as unused files in the `id_effect_cli`
package; no API, command or template ever read it (`id-effect new` uses its own templates in
`crates/id_effect_tool/templates/`). Nothing to change in code. For an admin, use the new
`id_effect_admin` crate (ADR 0010 Amendment 9) and `id-effect generate admin <Entity>`.

## AI: `id_effect_ai` published from `crates/` (was `labs/`)

Owner: r13-e9-1-promote. The crate moved from `labs/id_effect_ai` to `crates/id_effect_ai` (ADR 0010
Amendment 9) and is a root-workspace member. A path dependency on the labs crate changes its path:

```toml
# before (0.12, labs)
id_effect_ai = { path = "../id_effect/labs/id_effect_ai" }
# after (0.13)
id_effect_ai = { path = "../id_effect/crates/id_effect_ai" }   # or: id_effect_ai = "0.13"
```

The API changes of the promotion are listed here by r13-e9-1-promote.
