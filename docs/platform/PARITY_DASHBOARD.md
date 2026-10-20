# Platform parity dashboard

Living view of **Effect.ts parity** vs **id_effect** platform crates, as of 0.7 (crate tiers:
[ADR 0010](../adrs/0010-crate-tiers.md); every crate runs on the 0.7 kernel,
[ADR 0011](../adrs/0011-fiber-runtime-send-typed-r.md)). "Labs (unpublished)" means the code exists under
`labs/` and builds in its own CI job, but is not on crates.io and has no API guarantees.

## Crate status

| Phase | Target | Crate / module | Tier | Status |
|-------|--------|----------------|------|--------|
| A | `@effect/platform` | `id_effect_platform` | integration | Shipped — HTTP (`HttpClient`, reqwest 0.13 backend, streaming), `FileSystem`, subprocess `command` (was `process`), `uri`; session/OAuth traits behind the opt-in `auth` feature (`MemoryOAuthClient` needs `test-util`) |
| B | `@effect/opentelemetry` | `id_effect_opentelemetry` | integration | Shipped — span bridge (`OtelSpanHook` on `Effect::with_span`), metrics bridge over `MetricRegistry`, OTLP, logs bridge, W3C+baggage, shutdown |
| C | `@effect/sql` | `id_effect_sql` | integration | Shipped — `SqlClient`, `with_transaction`, `TestSqlClient`; Postgres via sqlx in `id_effect_sql::pg` (feature `pg`); live tests enforced in CI with `ID_EFFECT_REQUIRE_PG=1` |
| D | `@effect/rpc` | `id_effect_rpc` | labs (unpublished) | Correlation ids, JSON error envelopes, OpenAPI emission, versioning, tracing spans |
| E | `@effect/cli` | `id_effect_cli` | integration | Partial — exit-code mapping, `run_main` (`id-effect-diagnose` removed in 0.7: a missing capability is a compile error); no scaffolder (the stub generator was removed in 0.6; `examples/cli-minimal` is the template) |
| F | Supervision | `id_effect::Supervisor` (`coordination`), `id_effect_process`, `id_effect_node` | core / runtime | Shipped — one layering: core restart primitive, OTP process trees, distributed supervision |
| G | Cluster / workflow | Process Mesh (`id_effect_process` + `id_effect_node`); `id_effect_workflow` | runtime; integration (workflow) | Cluster runtime shipped in the mesh. Durable workflow shipped (0.11) — history journal replayed on resume on `id_effect_sql` (Postgres), leases with fencing tokens, activities and durable timers as `id_effect_jobs` jobs, signals, sagas; `MemoryWorkflowStore` for tests (the labs duroxide/SQLite crate was removed) |
| H | `@effect/ai` | `id_effect_ai` | labs (unpublished) | OpenAI, Anthropic, Cursor vendors; MCP server template |
| — | Jobs / messaging / events | `id_effect_jobs`, `id_effect_messaging`, `id_effect_events` | platform (jobs, messaging; the Ledger depends on them), integration (events) | Shipped (0.10) — Postgres `SKIP LOCKED` job queue with retries, unique keys and cron; `MessageBroker` with `MemoryBroker` and Redis Streams (`id_effect_redis` feature `streams`), transactional outbox, relay and idempotent inbox; `SqlEventJournal` over any `SqlClient` |
| — | HTTP host (Axum / tower) | `id_effect_axum` (feature `tower` for the `service` module) | integration | Shipped — `EffectExecutor` (bounded admission, `SaturationPolicy`, `DropPolicy`), request-scoped capability overrides |
| I | Maintenance | `docs/effect-ts-parity/CHECKLIST-upstream-effect.md` | — | Ongoing |

## Review cadence

1. Bump [`UPSTREAM-VERSION`](../../docs/effect-ts-parity/UPSTREAM-VERSION) when reviewing Effect.ts releases.
2. Run `moon run :parity-checklist` (validates checklist + version file).
3. File gaps under `platform-parity-hygiene` mission.

## Platform missions

Full mission table: [ROADMAP.md](./ROADMAP.md).
