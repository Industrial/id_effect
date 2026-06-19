# id_effect_events

Event sourcing, projections, and CQRS boundaries for [`id_effect`](https://github.com/Industrial/id_effect) programs.

- **[`EventStore`]** — append-only streams with [`MemoryEventStore`] and [`FileJournal`]
- **[`EventEnvelope`]** — metadata shell with [`Schema`] wire bridging
- **[`run_projection`]** — fold events into read models
- **[`CommandHandler`]** / **[`QueryHandler`]** — CQRS dispatch helpers

See the mdBook chapter *Events and projections* (`part5/ch23-00-events-and-projections.md`).
