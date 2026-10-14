# Platform parity dashboard

Living view of **Effect.ts parity** vs **id_effect** platform crates, as of the 0.6 consolidation
([ADR 0010](../adrs/0010-crate-tiers.md)). "Labs (unpublished)" means the code exists under
`labs/` and builds in its own CI job, but is not on crates.io and has no API guarantees.

## Crate status

| Phase | Target | Crate / module | Tier | Status |
|-------|--------|----------------|------|--------|
| A | `@effect/platform` | `id_effect_platform` | integration | Shipped — HTTP (`HttpClient`, reqwest 0.13 backend, streaming), `FileSystem`, subprocess `command` (was `process`), `uri`; session/OAuth traits behind the opt-in `auth` feature (`MemoryOAuthClient` needs `test-util`) |
| B | `@effect/opentelemetry` | `id_effect_opentelemetry` | integration | Shipped — OTLP, logs bridge, W3C+baggage, DI, shutdown |
| C | `@effect/sql` | `id_effect_sql` | integration | Shipped — `SqlClient`, `with_transaction`, `TestSqlClient`; Postgres via sqlx in `id_effect_sql::pg` (feature `pg`, formerly `id_effect_sql_pg`, now a 0.6-only shim); live tests enforced in CI with `ID_EFFECT_REQUIRE_PG=1` |
| D | `@effect/rpc` | `id_effect_rpc` | labs (unpublished) | Correlation ids, JSON error envelopes, OpenAPI emission, versioning, tracing spans |
| E | `@effect/cli` | `id_effect_cli` | integration | Partial — exit-code mapping, `run_main`, `id-effect-diagnose`; no scaffolder (the stub generator was removed in 0.6; `examples/cli-minimal` is the template) |
| F | Supervision | `id_effect::concurrency::Supervisor`, `id_effect_process`, `id_effect_node` | core / runtime | Shipped — one layering: core restart primitive, OTP process trees, distributed supervision |
| G | Cluster / workflow | Process Mesh (`id_effect_process` + `id_effect_node`); `id_effect_workflow` | runtime; labs (unpublished) | Cluster runtime shipped in the mesh. Durable workflow (duroxide-pg journal, bundled-SQLite dev log, fsm bridge) is labs |
| H | `@effect/ai` | `id_effect_ai` | labs (unpublished) | OpenAI, Anthropic, Cursor vendors; MCP server template |
| — | Messaging / events | `id_effect_jobs`, `id_effect_events`, `id_effect_graph` | labs (unpublished) | Apalis 1.0 queue, obix outbox/inbox, rdkafka broker, es-entity journal |
| — | HTTP host (Axum / tower) | `id_effect_axum` (absorbs `id_effect_tower` as feature `tower`) | integration | Shipped — `EffectExecutor` (bounded admission, `SaturationPolicy`, `DropPolicy`), request-scoped capability overrides |
| I | Maintenance | `docs/effect-ts-parity/CHECKLIST-upstream-effect.md` | — | Ongoing |

## Review cadence

1. Bump [`UPSTREAM-VERSION`](../../docs/effect-ts-parity/UPSTREAM-VERSION) when reviewing Effect.ts releases.
2. Run `moon run :parity-checklist` (validates checklist + version file).
3. File gaps under `platform-parity-hygiene` mission.

## Platform missions

Full mission table: [ROADMAP.md](./ROADMAP.md).
