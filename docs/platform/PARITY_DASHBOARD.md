# Platform parity dashboard

Living view of **Effect.ts parity** vs **id_effect** platform crates.

## Crate status

| Phase | Target | Crate / module | Status |
|-------|--------|----------------|--------|
| A | `@effect/platform` | `id_effect_platform` | Partial — wave 0 in progress |
| B | `@effect/opentelemetry` | `id_effect_opentelemetry` | Shipped — OTLP, logs bridge, W3C+baggage, DI, shutdown |
| C | `@effect/sql` | `id_effect_sql` | Missing |
| D | `@effect/rpc` | `id_effect_rpc` | Partial — codegen pending |
| E | `@effect/cli` | `id_effect_cli` | Partial — wave 0: `id-effect version/new` |
| F | Supervision | `id_effect::concurrency::supervisor` | Shipped |
| G | Cluster / workflow | `id_effect_workflow` | Partial — SQLite spike |
| H | `@effect/ai` | `id_effect_ai` | Partial — OpenAI, Anthropic, Cursor vendors |
| I | Maintenance | `docs/effect-ts-parity/CHECKLIST-upstream-effect.md` | Ongoing |

## Review cadence

1. Bump [`UPSTREAM-VERSION`](../../docs/effect-ts-parity/UPSTREAM-VERSION) when reviewing Effect.ts releases.
2. Run `moon run :parity-checklist` (validates checklist + version file).
3. File gaps under `platform-parity-hygiene` mission.

## Platform missions

Full mission table: [ROADMAP.md](./ROADMAP.md).
