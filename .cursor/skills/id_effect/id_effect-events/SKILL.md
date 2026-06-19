---
name: id_effect-events
description: >-
  Write and review id_effect_events and id_effect_graph — EventStore, MemoryEventStore,
  FileJournal, EventEnvelope with Schema, run_projection, CommandHandler/QueryHandler,
  Dag and topological_sort. Use when editing event sourcing or Part V ch23 book content.
---

# id_effect-events

**Crates:** `crates/id_effect_events`, `crates/id_effect_graph` · **Book:** Part V ch23

## Core API

| Type | Use |
|------|-----|
| `EventStore::append` / `read` | Append-only stream persistence |
| `MemoryEventStore` | In-memory store (tests, demos) |
| `FileJournal` | JSON-lines journal on disk |
| `EventEnvelope` + `envelope_schema` | Metadata + Schema wire bridge |
| `Projection` + `run_projection` | Fold events into read models |
| `CommandHandler` + `dispatch_command` | CQRS write side |
| `QueryHandler` + `query_projection` | CQRS read side |
| `Dag` / `DependencyNode` + `topological_sort` | DAG build ordering |

## Rules

- **Streams are append-only** — never mutate stored events; rebuild projections from the log.
- **Versions are 1-based** — `read(stream, 1)` returns the full stream.
- **Schema at the boundary** — encode envelopes with `to_wire` / `envelope_schema` before RPC or file export.
- **Projections are pure folds** — keep `apply` deterministic; run effects in command handlers only.

## Verify

```bash
cargo test -p id_effect_events
cargo test -p id_effect_graph
cargo clippy -p id_effect_events -p id_effect_graph -- -D warnings
```

## See also

- `id_effect-schema` — `HasSchema`, combinators
- `id_effect-resilience` — `SubscriptionRef` for live projection updates
- `id_effect-capabilities` — planner uses the same sort as `id_effect_graph`
