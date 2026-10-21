# ADR 0010 — Vision and crate tiers

## Status

Accepted (2026-10-14). Executed by mission `id-effect-consolidation-0-6`
(`.maestro/missions/id-effect-consolidation-0-6.plan.md`). Amended for 0.7 on
2026-10-15 (see [Amendment 3](#amendment-3-2026-10-15-for-07)).
Superseded in part by [ADR 0012](0012-platform-vision.md) (vision statement and Rule 4).

## Context

By 0.5.0 the workspace had grown to 25 crates and roughly 90k lines. A module-fit
audit (2026-10-13) against the two serious consumers — solana-yield-optimizer
(recorder on `id_effect_process`) and assay (capability testing kit, `run_blocking`,
`id_effect_cli` exit codes) — found:

- Roughly a third of the code has zero downstream users: the core fp modules
  (algebra, foundation, collections, stm), and the jobs, workflow, events, rpc, parse
  and ai crates.
- Several crates export stubs or spikes from their public API (`derive(Fsm)`,
  `KafkaBrokerStub`, `NetworkJournalStub`, `SchemaBridgeStub`, a deterministic OAuth
  client, `Monad::flatten` calling `unimplemented!`).
- Concepts are duplicated: four types named *Supervisor*, two topological sorts,
  `Bulkhead` vs `Semaphore`, two transducers, three observability layers, two
  node-identity types, two outbox implementations, and a workflow wire protocol
  alongside `id_effect_node`.
- Heavy optional dependencies (rdkafka, bundled rusqlite, apalis release
  candidates) shape main CI, with OpenSSL installs and a Windows special case, for
  crates nobody uses.
- The consumers hit runtime defects in core: `run_with` replaces an installed
  fabric, sysinfo telemetry is sampled eagerly, and streams are not pull-based.

Breadth has outrun quality. New features compete with hardening the parts that
people actually use.

## Decision

### Vision

> id_effect is a typed effect runtime for Rust — `Effect<A, E, R>`, capability DI,
> structured concurrency, resilience, pull-based streams and a first-class testing
> kit — plus OTP-grade supervision and distribution, for long-running supervised
> services, daemons and CLIs. It is not a batteries-included web framework.

Every crate has to justify itself against this sentence.

### Tiers

Every workspace package declares its tier in its manifest:

```toml
[package.metadata.id_effect]
tier = "core"   # core | runtime | integration | maintenance | labs | merge | example
```

| Tier | Published | Bar | Crates (target state) |
|---|---|---|---|
| `core` | yes | strict | id_effect (slimmed), id_effect_macro, id_effect_proc_macro, id_effect_tokio, id_effect_config, id_effect_logger, id_effect_resilience |
| `runtime` | yes | strict | id_effect_process, id_effect_node, id_effect_fsm (reworked as a gen_statem-style behaviour) |
| `integration` | yes | standard | id_effect_platform, id_effect_axum (absorbs tower), id_effect_sql (absorbs sql_pg as a `pg` feature), id_effect_opentelemetry, id_effect_cli |
| `maintenance` | yes | frozen | id_effect_optics (assay depends on it), id_effect_fp (new: algebra, foundation, collections and stm moved out of core) |
| `labs` | **no** (`publish = false`, under `labs/`) | builds only | id_effect_rpc, id_effect_ai, id_effect_parse, id_effect_lint, and the durable family: id_effect_workflow, id_effect_events, id_effect_jobs, id_effect_graph |
| `merge` | transitional | none | id_effect_tower → axum, id_effect_sql_pg → sql (`pg` feature) |
| `example` | no | builds | examples/cli-minimal |

#### Amendment 1 (2026-10-14, during W2 planning)

- `id_effect_jobs` moves to labs whole instead of being split. No published crate
  needs its fabric runner, and splitting it would cost more than it delivers while
  nothing consumes it.
- `id_effect_graph` moves to labs rather than merging into the core planner. Its
  only consumer is `id_effect_events`, so leaving the published surface removes the
  duplicate toposort just as well.
- The durable family (workflow, events, jobs, graph) stays as four labs crates. A
  merged `id_effect_durable` is deferred until a consumer promotes it (rule 2).
- `labs/` is a **separate Cargo workspace** with path dependencies on `crates/`.
  Main-workspace commands (`cargo --workspace`, coverage, the CI matrix,
  minimal-versions) never build labs dependencies such as rdkafka, OpenSSL,
  bundled SQLite or apalis. A dedicated CI job builds and tests labs.
- `id_effect_fsm`'s durable bridge (`fsm/src/workflow.rs`) moves into
  `labs/id_effect_workflow`, so the runtime-tier fsm crate has no labs dependency.

#### Amendment 2 (2026-10-14, after w2-core-slim)

- `id_effect_fp` exists (maintenance tier). It holds `algebra`, `collections`,
  `stm`, `kernel::{result, reader, thunk}` and the `foundation` pieces core does not
  use. The `foundation` parts core uses throughout (`either`, `func`, `piping`,
  `predicate`, `mutable_ref`) stay in core.
- Placement (`compute/cluster.rs`) moved into `id_effect_node::placement`; core
  never used it.
- **Deviation:** tokio remains a required core dependency. The coordination
  primitives (Queue, Deferred, Semaphore, PubSub, …) are built on `tokio::sync`, and
  `FiberHandle` combinators spawn on Tokio. The features are cut to
  `macros, rt, sync`. Making tokio optional would mean a runtime-agnostic
  coordination layer, which is post-gate extension work.
- `im` was removed from core rather than made optional. One consequence is that
  `Cache` now clones a std map on every modify, where `im` used structural sharing.
  This is tracked as a performance follow-up.

#### Amendment 3 (2026-10-15, for 0.7)

Records the 0.7 state (mission `id-effect-runtime-0-7`, task `w2-adr-0010-amend`;
release plan §9, rulings R4–R6). Where earlier sections disagree with this one,
this one wins.

- **The `merge` tier is empty.** `id_effect_tower` and `id_effect_sql_pg` are
  deleted (commit `683ea84`), as rule 3 prescribes after their one minor release of
  shims. Use `id_effect_axum` with the `tower` feature and `id_effect_sql` with the
  `pg` feature.
- **17 published crates:** 7 core, 3 runtime, 5 integration and 2 maintenance, as in
  the table above minus the `merge` row. No tier changes.
- **tokio stays mandatory (R6).** This closes the Amendment 2 deferral ("making
  tokio optional … is post-gate extension work") as *not doing it*:
  - The 0.7 fiber runtime runs on an `Executor` trait, and its default backend is
    tokio. Core, the coordination primitives and the integration crates all assume
    that backend.
  - An optional tokio would need a cfg matrix (with and without tokio) across core
    and every dependent crate, in CI and in the docs, for no current consumer.
  - The `Executor` trait keeps the door open. Another backend can be added behind
    it later without making tokio optional first.
- **Labs migrate to 0.7 (R5).** Every labs crate moves to the 0.7 kernel with the
  rest of the workspace. They get no new features, and they stay unpublished under
  rule 4.
- **`spikes/` is temporary.** It is a standalone Cargo workspace like `labs/`, not a
  root-workspace member, and its crates are `publish = false`. It holds the two
  kernel design spikes for 0.7 and is deleted once the chosen kernel lands. It has no
  tier.
- **`examples/http-showcase` is tier `example`** (`publish = false`), alongside
  `examples/cli-minimal`.
- **The coverage floor rises to 96 % (R4)** on core, runtime and integration crates.
  It replaces the 95 % in rule 4. The script and `docs/QUALITY_BAR.md` switch to
  96 % at the end of the 0.7 migration, once the old and new kernels no longer
  coexist (task `r-coverage-floor`).

#### Amendment 4 (2026-10-16, for 0.8)

Names the tiers of the two 0.8 front-door crates (ADR 0012 L3, L11), as rule 5 requires.

- **`id_effect_kit` is tier `integration`.** It is the facade crate: one dependency, a
  feature flag per battery (`web`, `sql-pg`, `otel`, `cluster`, `full`), whole-crate
  re-exports and a `prelude`. It belongs in the `platform` tier of ADR 0012, but that tier's
  extra gate (a CI-tested reference app under `examples/reference/` depends on it) cannot
  pass until the reference apps exist. It moves to `platform` then, with no other change.
  It publishes last, after every crate it re-exports.
- **`id_effect_tool` is tier `integration`.** It publishes the `id-effect` binary (`new`,
  `generate`, `dev`, `doctor`, `migrate`, `deploy`) and publishes after `id_effect_kit`.
- Neither crate has a crates.io release yet, so both are excluded from the semver job
  until their first publish gives it a baseline.

#### Amendment 5 (2026-10-16, for 0.9)

Names the tiers of the 0.9 platform-battery crates (ADR 0012 pre-lists them; release spec
0.9.0 decision D8), as rule 5 requires.

- **`id_effect_authz` is tier `integration`.** Authorization: role and permission sets, named,
  resource and attribute policies, route guards, allow/deny test helpers. It defines the
  `Principal` trait.
- **`id_effect_auth` is tier `integration`.** Authentication: argon2id passwords, persistent
  sessions, bearer/JWT (JWKS), basic and API-key credentials, OIDC + PKCE, TOTP. Its principal
  implements `id_effect_authz::Principal`, so it publishes after `id_effect_authz`.
- **`id_effect_test` is tier `integration`.** Integration-test support: effect-native
  containers as scoped layers, per-test database isolation, fixtures and the in-process HTTP
  client.
- All three belong in the `platform` tier of ADR 0012. Each moves there, with no other change,
  when the Ledger reference app under `examples/reference/` depends on it (check P of
  `scripts/tier-conformance.sh`).
- They publish at the last level, after `id_effect_axum` and `id_effect_sql`, in the order
  `id_effect_authz`, `id_effect_auth`, `id_effect_test`, and before `id_effect_kit` and
  `id_effect_tool` (`scripts/validate-crate-packaging.sh --print-order`).
- None has a crates.io release yet, so all three are excluded from the semver job until their
  first publish gives it a baseline. No placeholder versions are published: each crate's first
  release carries its API.
- **`id_effect_redis` is tier `integration`** (added when the 0.9 stretch epic E3.4 shipped).
  Redis over redis-rs: the pooled client layer, typed key-value, hash and JSON stores, the
  `CacheBackend` behind the core `Cache`, pub/sub as a `Stream`, and, behind features,
  `id_effect_auth`'s `SessionStore` and `id_effect_axum`'s `RateLimitStore`. It depends on those
  two crates (never the reverse), so it publishes after `id_effect_auth`, before
  `id_effect_test`. It moves to `platform` on the same rule as the three above, and is excluded
  from the semver job until its first publish. Audit logging is a feature of `id_effect_sql` and
  OpenAPI a feature of `id_effect_axum`, so neither adds a crate.

#### Amendment 6 (2026-10-17, for 0.10)

Names the tiers of the six new published crates of 0.10 (release spec 0.10.0 decisions D2 and
D5), as rule 5 requires. All six are tier `integration`.

- **`id_effect_jobs` is tier `integration`.** Background jobs: the Postgres `SKIP LOCKED` queue on
  `id_effect_sql` (retries with backoff on the core `Schedule`, a `dead` state, unique keys, named
  queues and priorities, `run_at`, enqueue inside an `SqlTransaction`), workers with per-queue
  concurrency, and cron schedules run once per cluster through `id_effect_node`. It replaces the
  labs crate of the same name; apalis, obix, `job` and rdkafka do not come with it.
- **`id_effect_messaging` is tier `integration`.** The broker-agnostic `MessageBroker` (publish,
  subscribe, ack), an in-memory broker, and the transactional outbox, inbox and relay on
  `id_effect_sql`. It is separate from `id_effect_jobs` so that `id_effect_redis`, whose `streams`
  feature implements the broker on Redis Streams, stays off `id_effect_node`.
- **`id_effect_mail` is tier `integration`.** Outgoing mail: SMTP over lettre, the SendGrid v3 API
  over the HTTP client, minijinja templates, and delivery through `id_effect_jobs`.
- **`id_effect_console` is tier `integration`.** The operator console: live fibers from the core
  `fiber-registry` feature, jobs and queues, behind `id_effect_authz` guards.
- **`id_effect_storage` is tier `integration`.** Object storage: one store interface over the local
  file system, in-memory and S3-compatible backends.
- **`id_effect_events` is tier `integration`.** Event sourcing (event store, `SqlJournal` with
  expected-version appends, projections, snapshots, checkpointed subscriptions), promoted from
  `labs/` under rule 2 by leaf `r10-e7-4-promote`. Until that leaf lands it is still the labs crate.
- They publish in dependency order (`scripts/validate-crate-packaging.sh --print-order`):
  `id_effect_events`, `id_effect_messaging` and `id_effect_storage` after `id_effect_sql` and
  `id_effect_platform`; `id_effect_jobs` after `id_effect_node`; `id_effect_mail` after
  `id_effect_jobs` and `id_effect_auth`; `id_effect_console` after `id_effect_jobs` and
  `id_effect_authz`. All six publish before `id_effect_kit` and `id_effect_tool`.
- The Ledger reference app moves `id_effect_jobs`, `id_effect_messaging`, `id_effect_mail`,
  `id_effect_console` and `id_effect_storage` to the `platform` tier of ADR 0012 (check P of
  `scripts/tier-conformance.sh`), with no other change. `id_effect_events` stays `integration`
  unless the Ledger uses it.
- crates.io (checked 2026-10-17): `id_effect_messaging`, `id_effect_mail`, `id_effect_console` and
  `id_effect_storage` are free. `id_effect_jobs` and `id_effect_events` are ours, last published at
  0.4.0 before they moved to labs, so 0.10.0 is their next release. None of the six has a 0.9.0
  baseline, so all are excluded from the semver job until their 0.10.0 publish. No placeholder
  versions are published: each crate's first release carries its API.

#### Amendment 7 (2026-10-18, for 0.11)

Names the tier of the new published crate of 0.11, the STM move and the unpublished security test
crate (release spec 0.11.0 decisions D5 and D6), as rule 5 requires. Where the table above disagrees
with this amendment, this amendment wins.

- **`id_effect_workflow` is tier `integration`.** Durable workflows on its own Postgres engine: a
  history journal replayed on resume on `id_effect_sql`'s `SqlClient`/`SqlTransaction`, one lease
  holder per instance with a fencing token, activities as `id_effect_jobs` jobs, durable timers
  through `run_at`, signals, sagas, and a `MemoryWorkflowStore` for tests. It replaces
  `labs/id_effect_workflow`, which leaf `r11-e7-5-sagas-promote` deletes; duroxide, duroxide-pg,
  rusqlite and the `NetworkJournalStub` do not come with it. It leaves the `labs` row of the table.
- It publishes at the last level, after `id_effect_sql` and `id_effect_jobs` (and after
  `id_effect_mail` and `id_effect_console`), and before `id_effect_kit` and `id_effect_tool`
  (`scripts/validate-crate-packaging.sh --print-order`). It moves to the `platform` tier of
  ADR 0012, with no other change, when the Ledger reference app depends on it (check P of
  `scripts/tier-conformance.sh`).
  - *Note (2026-10-19):* that happened in 0.11 — the Ledger runs account closure as a durable
    workflow on it — so `id_effect_workflow` is now tier `platform` (ADR 0012 rule 4). The
    first bullet of this amendment records the tier it entered with.
- crates.io (checked 2026-10-18): `id_effect_workflow` is ours, last published at 0.4.0 (the
  SQLite/duroxide step log) before it moved to labs, so 0.11.0 is its next release. It has no
  0.10.x baseline, so it is excluded from the semver job until its 0.11.0 publish.
- **STM moves into core.** `id_effect::stm` is an always-compiled core module (std only, no new
  dependency); `id_effect_fp` (maintenance) keeps `id_effect_fp::stm` as a `#[deprecated]`
  re-export for one minor release (rule 3), then drops it. This reverses the `stm` part of
  Amendment 2; the rest of `id_effect_fp` is unchanged.
- **`tests/security_compile_fail` is tier `example`** and `publish = false`: the compile-fail
  security cases of ADR 0013. It is a root-workspace member outside `crates/`, so it is never in
  the publish order, the semver job or `publish.yml`.

#### Amendment 8 (2026-10-19, for 0.12)

Names the tiers of the two published RPC crates of 0.12, the Relay reference app and the move of
the TLS certificate code into `id_effect_platform` (release spec 0.12.0 decisions D3 and D6,
ADR 0014 "One call surface"), as rule 5 requires. Where the table above disagrees with this
amendment, this amendment wins.

- **`id_effect_rpc` leaves the `labs` row and enters tier `integration`.** RPC is the first-party
  call surface (ADR 0014): typed procedures over HTTP, WebSocket and HTTP/3, batching, two-way
  streams with resume cursors, and channels as its `id_effect_rpc::channels` module (there is no
  separate channels crate). GraphQL is not built (a non-goal since scorecard method 2.2). The
  crate moved with its history (`git mv labs/id_effect_rpc crates/id_effect_rpc`, leaf
  `r12-t0-skeletons`) and is a root-workspace member. Until leaf `r12-e12-1-promote` brings it to
  the integration bar it stays `publish = false` in the transitional `merge` tier (no bar; the
  `labs` tier is for crates under `labs/` only), listed in `PENDING_PROMOTION` of
  `scripts/validate-crate-packaging.sh`; that leaf sets `integration`, makes it publishable and
  removes the entry. It moves to the `platform` tier of ADR 0012, with no other change, when the
  Relay reference app depends on it (check P of `scripts/tier-conformance.sh`).
- **`id_effect_rpc_macros` is tier `integration`.** The `#[rpc::service]` proc-macro crate
  (procedure kinds, access decisions, the generated client and contract). It is a separate crate
  because a proc-macro crate cannot export runtime items; `id_effect_rpc` re-exports it.
- They publish in dependency order (`scripts/validate-crate-packaging.sh --print-order`):
  `id_effect_rpc_macros` with the other macro crates (it depends on no workspace crate), and
  `id_effect_rpc` after `id_effect_axum`, `id_effect_platform`, `id_effect_node` and
  `id_effect_events` (and the other level-5 crates), before `id_effect_kit` and `id_effect_tool`.
- **`id_effect_platform` gains the features `tls`, `quic`, `http3` and `acme`** (leaves of E12.3):
  rustls configuration and certificate loading/reloading, the one QUIC endpoint shared by HTTP/3
  and the mesh (ALPN routing), the HTTP/3 client, and TLS-ALPN-01 certificates. No tier change.
- **The node certificate code moves into `id_effect_platform::tls`.**
  `crates/id_effect_node/src/transport/certs.rs` (`NodeCertificate`, `CertReloader`, `Identity`,
  the client and server TLS configs) moves there, so the axum TLS/HTTP/3 server, the HTTP/3 client
  and the mesh share one implementation. `id_effect_node::transport::certs` keeps a
  `#[deprecated]` re-export for one minor release (rule 3, L9), then drops it.
- **`examples/reference/relay` is tier `example`** and `publish = false`: the 0.12 reference app
  (3-node chat with presence over two-way RPC, channels and the mesh, over HTTP/3 and WebSocket).
  It depends on `id_effect_kit` only, like the Ledger.
- crates.io (checked 2026-10-19, generic User-Agent): `id_effect_rpc` is ours, last published at
  0.4.0 before it moved to labs, so its promoted release is its next one. `id_effect_rpc_macros`
  is free; like any never-published crate it needs its crates.io trusted-publisher setup before
  `publish.yml` can release it (docs/RELEASING.md). Neither has a 0.11.x baseline, so both are
  excluded from the semver job until their first 0.12 publish. No placeholder versions are
  published: each crate's first release carries its API.

#### Amendment 9 (2026-10-21, for 0.13)

Names the tiers of the three crates of 0.13 "Breadth" and of the Pipeline reference app (release
spec 0.13.0 decisions D1, D2, D3, D5 and D6), as rule 5 requires. Where the table above disagrees
with this amendment, this amendment wins.

- **`id_effect_ai` leaves the `labs` row and enters tier `integration`.** Provider-neutral chat
  (OpenAI, Anthropic) with streaming, typed tools and a bounded agent loop, structured output,
  embeddings (OpenAI, Ollama) and an effect-native MCP server and client; all HTTP goes through
  `id_effect_platform`'s client. The Cursor agents module stays behind the non-default `cursor`
  feature. The crate moved with its history (`git mv labs/id_effect_ai crates/id_effect_ai`, leaf
  `r13-t0-skeletons`; manifest paths only) and is a root-workspace member. Until leaf
  `r13-e9-1-promote` brings it to the integration bar it stays `publish = false` in the
  transitional `merge` tier (no bar; the `labs` tier is for crates under `labs/` only), listed in
  `PENDING_PROMOTION` of `scripts/validate-crate-packaging.sh`; that leaf sets `integration`,
  makes it publishable and removes the entry.
- **`id_effect_i18n` is tier `integration`** (new). Project Fluent message catalogs with CLDR
  plural rules, a per-request / per-fiber locale, `Accept-Language` negotiation and translated
  `Schema` messages.
- **`id_effect_admin` is tier `integration`** (new). The back office: `AdminSite` registration of
  `id_effect_sql` entities, list/detail/edit pages rendered with `id_effect_axum`'s `ui` feature,
  per-entity × action permissions through `id_effect_authz`, mounted only behind a guard. Generated
  code alone could not be published and tested, and `id_effect_axum` must not depend on
  `id_effect_sql`, so it is a crate of its own.
- **Server-rendered UI adds no crate.** It is the `ui` feature of `id_effect_axum` (maud
  components, htmx helpers, layouts, form helpers); no tier change.
- **`examples/reference/pipeline` is tier `example`** and `publish = false`: the 0.13 reference app
  (a multi-tenant document-intake pipeline and its back office). It depends on `id_effect_kit`
  only, like the Ledger and the Relay. `examples/recipes/` (the Leptos SSR recipe) is outside the
  root workspace, like `labs/`, with its own CI job.
- They publish in dependency order (`scripts/validate-crate-packaging.sh --print-order`), all at
  the last level after `id_effect_rpc` and before `id_effect_kit` and `id_effect_tool`:
  `id_effect_i18n` after `id_effect` and `id_effect_axum`; `id_effect_ai` after
  `id_effect_platform` and `id_effect_config`; `id_effect_admin` after `id_effect_sql`,
  `id_effect_axum`, `id_effect_authz` and `id_effect_i18n`.
- Each of the three moves to the `platform` tier of ADR 0012, with no other change, when the
  Pipeline depends on it (check P of `scripts/tier-conformance.sh`).
- `crates/id_effect_cli/templates/admin-stub/` is deleted: nothing generated, built or tested it,
  and `id_effect_admin` replaces it. Its only references were scorecard evidence text, re-scored at
  the 0.13 exit.
- crates.io (checked 2026-10-21, generic User-Agent): `id_effect_ai` is ours, last published at
  0.4.0 before it moved to labs, so its promoted release is its next one. `id_effect_admin` and
  `id_effect_i18n` are free; like any never-published crate each needs its crates.io
  trusted-publisher setup before `publish.yml` can release it (docs/RELEASING.md; owner step D17,
  before the v0.13.0 tag). None has a 0.12.x baseline, so all three are excluded from the semver
  job until their first 0.13 publish. No placeholder versions are published: each crate's first
  release carries its API.

The requirements for each tier are in [`docs/QUALITY_BAR.md`](../QUALITY_BAR.md).
`scripts/tier-conformance.sh` checks them.

### Rules

1. A crate enters `core`/`runtime`/`integration` only if it passes the conformance
   check at that tier.
2. A `labs` crate is promoted only when a real consumer depends on it and it passes
   the bar for its target tier.
3. Merges ship `#[deprecated]` re-export shims for one minor release, and the old
   crate is then removed.
4. Labs crates are excluded from `publish.yml`, the 95 % coverage floor and the
   default CI matrix. They build in a separate CI job.
5. New published crates require an ADR amendment that names their tier.

## Consequences

- The published surface shrinks from 25 crates to 15 (plus 2 maintenance), and most
  heavy optional dependencies leave main CI.
- Downstream sees one breaking release (0.6.0). Deprecation shims and a migration
  guide soften it. 0.5.0 is not published (decision D4).
- Durable execution (workflow, events, outbox) leaves the supported surface. If a
  consumer appears, it returns through rule 2.
- Extension work resumes only after the mission's exit gate: all tier 0–2 crates
  conform, coverage is at least 95 % on those tiers, 0.6.0 is published, and assay
  and solana-yield-optimizer build against it.

## Alternatives considered

- **Keep everything, harden everything.** Rejected: the hardening cost is spread
  across code with no users, and the CI weight stays.
- **Separate `id_effect-labs` repository.** Rejected for now (decision D3). Changes
  to core APIs would then need coordinated edits across repositories. An in-repo
  `labs/` folder keeps the history and keeps refactors atomic.
- **Delete unused crates outright.** Rejected: labs keeps the work recoverable at
  near-zero cost.
