# id_effect Cursor Skills

Project skills grounded in the **id_effect book** (`crates/id_effect/book/`), workspace examples (`crates/id_effect/examples/`), and ADRs (`docs/adrs/`).

## When to invoke

| Skill | Use when |
|-------|----------|
| [id_effect](SKILL.md) | Quick router — 3.0 DI cheat sheet, removed APIs, verify commands |
| [id_effect-fundamentals](id_effect-fundamentals/SKILL.md) | `Effect<A,E,R>`, laziness, `effect!`, `~`, map/flat_map, composition |
| [id_effect-capabilities](id_effect-capabilities/SKILL.md) | `#[capability]`, `caps!`, `ProviderSpec`, `run_with`, provider graphs |
| [id_effect-errors](id_effect-errors/SKILL.md) | `Exit`, `Cause`, recovery, error accumulation, CLI exit codes |
| [id_effect-concurrency](id_effect-concurrency/SKILL.md) | Fibers, `fiber_all`, cancellation, `FiberRef`, scopes, `Schedule` |
| [id_effect-streams](id_effect-streams/SKILL.md) | `Stream`, `Sink`, chunks, backpressure, Rayon `Parallelism` |
| [id_effect-schema](id_effect-schema/SKILL.md) | `Unknown`, schema combinators, validation, parse errors at boundaries |
| [id_effect-optics](id_effect-optics/SKILL.md) | Lens, Prism, Traversal, schema paths, JSON patch — Part V ch18 |
| [id_effect-parse](id_effect-parse/SKILL.md) | Parser combinators, Pretty, Codec, Diff, Stream parse bridges |
| [id_effect-testing](id_effect-testing/SKILL.md) | `run_test`, `TestClock`, `mock_capability!`, property tests |
| [id_effect-integration](id_effect-integration/SKILL.md) | `id_effect_tokio`, `_platform`, `_axum`, `_reqwest`, `_config`, `_cli` |
| [id_effect-events](id_effect-events/SKILL.md) | EventStore, projections, CQRS, DAG — Part V ch23 |

| [id_effect-algebra](id_effect-algebra/SKILL.md) | Foldable, Alternative, Traversable, Bifoldable |
| [id_effect-optics](id_effect-optics/SKILL.md) | Lens, Prism, Traversal, Schema paths |
| [id_effect-fsm](id_effect-fsm/SKILL.md) | Typed FSM, saga, session types |
| [id_effect-parse](id_effect-parse/SKILL.md) | Parser combinators, Pretty, Diff |
| [id_effect-resilience](id_effect-resilience/SKILL.md) | Circuit breaker, rate limit, hedged requests |
| [id_effect-events](id_effect-events/SKILL.md) | EventStore, projections, CQRS |
| [id_effect-review](id_effect-review/SKILL.md) | PR review, idiomaticity, DI violations, pre-merge gate |

## Prerequisites chain

```
id_effect-fundamentals
  → id_effect-capabilities
    → id_effect-errors | id_effect-concurrency | id_effect-integration
      → id_effect-streams | id_effect-schema | id_effect-parse | id_effect-testing
```

Skills cross-link at boundaries. Capability DI lives in `id_effect-capabilities`; parallelism policy in `id_effect-streams`; test harness in `id_effect-testing`.

## Canonical sources

- Book: `crates/id_effect/book/` — build with `cd crates/id_effect/book && mdbook build`
- Examples: `crates/id_effect/examples/` — numbered progression 001–085+
- Migration: `book/src/appendix-b-migration.md` (async fn, 1.x DI, 2.x → 3.0)
- ADRs: `docs/adrs/0002-*` through `0006-*`

## Platform Kitchen Sink missions

When working on **Part VI–VII** or `platform-*` Maestro missions, start with [docs/platform/ROADMAP.md](../../../docs/platform/ROADMAP.md).

| Mission | Primary skills |
|---------|----------------|
| `platform-foundation` | `id_effect-integration`, `id_effect-streams` |
| `platform-observability` | `id_effect-integration` |
| `platform-data` | `id_effect-integration`, `id_effect-schema` |
| `platform-api-boundaries` | `id_effect-schema`, `id_effect-integration` |
| `platform-application` | `id_effect-integration`, `id_effect-capabilities` |
| `platform-async-messaging` | `id_effect-events`, `id_effect-concurrency` |
| `platform-workflow-cluster` | `id_effect-fsm`, `id_effect-resilience` |
| `platform-dx-ship` | `id_effect-integration` |
| `platform-ai` | `id_effect-integration`, `id_effect-streams` |
| `platform-parity-hygiene` | `id_effect-review` |

Prerequisites: `id_effect-fundamentals` → `id_effect-capabilities` → `id_effect-integration`.
| [id_effect-platform](id_effect-platform/SKILL.md) | `id_effect_platform` HTTP/FS/process — Part VI ch26 |
