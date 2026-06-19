---
name: id_effect-events
description: >-
  Write and review id_effect_events and id_effect_graph — EventStore, EsEntityEventStore,
  ProjectionRunner, MemoryEventStore, FileJournal, CQRS dispatch, Dag and topological_sort.
  Use when editing event sourcing or Part V ch23 book content.
---

# id_effect-events

**Crates:** `crates/id_effect_events`, `crates/id_effect_graph` · **Book:** Part V ch23

## Core API

| Type | Use |
|------|-----|
| `EsEntityEventStore` / `EsEntityPgBackend` | Production PG journal (feature `es-entity`) |
| `provide_es_entity_events` | Capability provider for shared journal |
| `ProjectionRunner` + `ProjectionNode` | Multi-projection rebuild order via graph |
| `dispatch_command_es_entity` | Async CQRS write + project |
| `EventStore::append` / `read` | Append-only stream persistence |
| `MemoryEventStore` / `FileJournal` | Dev/test stores |
| `CommandHandler` + `dispatch_command` | Sync CQRS (in-memory / file) |
| `DependencyNode` + `topological_sort` | DAG ordering (`id_effect_graph`) |

## Rules

- **Production PG** — use `es-entity` feature; do not add raw `PgSqlJournalBackend` paths.
- **Streams are append-only** — rebuild projections from the log.
- **Versions are 1-based** — `read(stream, 1)` returns the full stream.
- **Graph plans only** — `id_effect_graph` does not store events.

## Verify

```bash
cargo test -p id_effect_events --features es-entity
cargo test -p id_effect_graph
cargo clippy -p id_effect_events -p id_effect_graph -- -D warnings
```

## See also

- `id_effect-resilience` — `SubscriptionRef` for live projection updates
- Part VI ch31 — obix outbox after event append
- Part VI ch32 — duroxide `StepJournal`
