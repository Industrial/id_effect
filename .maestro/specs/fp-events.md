---
title: FP Events, Graph Projections, and Duroxide Workflow
slug: fp-events
mode: heavy
work_type: initiative
risk_class: medium
version: 2
acceptance_criteria:
  - "Production event persistence via es-entity DbOp on shared PgPoolKey (EsEntityPgBackend, provide_es_entity_events)"
  - "ProjectionRunner uses id_effect_graph topological_sort for multi-projection rebuild order"
  - "dispatch_command_es_entity persists command events in one DbOp transaction"
  - "Production workflow via duroxide + duroxide-pg StepJournal and DuroxideWorkflowRuntime on PgPoolKey"
  - "id_effect_fsm register_fsm / step_durable / restore_state generic over impl StepJournal"
  - "E2E example: command → es-entity → graph projections → obix outbox → duroxide step (DATABASE_URL)"
  - "Book ch23 (es-entity + ProjectionRunner), ch32 (duroxide); id_effect-events skill updated"
  - "Legacy PgSqlJournalBackend production path removed; id_effect_graph proptest for topological_sort"
  - "Workspace tests, clippy, coverage, book pass"
non_goals:
  - Effect.ts parity work
  - Temporal SDK in-crate
  - esrs / eventastic / autumn-harvest adoption
  - id_effect_graph as event store
---

# FP Events, Graph Projections, and Duroxide Workflow

Revise the fp-events initiative in place: **es-entity** for production event persistence, **id_effect_graph** for projection ordering, **duroxide-pg** for production durable workflow. Keep `id_effect_events` and `id_effect_workflow` as thin Effect facades.

See `docs/fp-patterns/ROADMAP.md`, `.cursor/plans/es_entity_duroxide_graph.plan.md`, and ADRs:

- [adr-es-entity-event-persistence.md](../../docs/platform/adrs/adr-es-entity-event-persistence.md)
- [adr-workflow-charter.md](../../docs/platform/adrs/adr-workflow-charter.md)

## Architecture

- **Events track:** `dispatch_command_es_entity` → `EsEntityPgBackend` / `SqlEventJournal` → `ProjectionRunner::plan` (`id_effect_graph`) → optional obix outbox (feature `outbox` on jobs).
- **Workflow track:** `DuroxideStepJournal` + `DuroxideWorkflowRuntime` on shared pool; FSM bridge over `StepJournal`.
- **Breaking:** Remove `PgSqlJournalBackend` and `POSTGRES_JOURNAL_DDL` production exports; SQLite workflow behind `memory` feature only.
