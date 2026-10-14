# FP Kitchen Sink Roadmap

Functional-pattern expansion for id_effect (excluding Effect.ts parity).

> **Status (0.6): all missions below are completed.** This file is a historical index. What
> happens next is set by the 0.6 consolidation plan,
> [`.maestro/missions/id-effect-consolidation-0-6.plan.md`](../../.maestro/missions/id-effect-consolidation-0-6.plan.md),
> and [ADR 0010](../adrs/0010-crate-tiers.md). The algebra, collections and STM code moved out of
> core into the maintenance-tier (frozen API) `id_effect_fp` crate; `id_effect_parse` and
> `id_effect_events` / `id_effect_graph` moved to the unpublished `labs/` workspace.

## Missions (completed)

| Execution overlay | Mission ID | Spec | Book | 0.6 status |
|------|------------|------|------|------------|
| [fp_algebra_foundation](../../.maestro/missions/fp-algebra.execution.md) | `pln-mqjykorq-bvmr7a` | fp-algebra | Part V ch17 | done — now `id_effect_fp::algebra` (maintenance) |
| [fp_optics_immutable](../../.maestro/missions/fp-optics.execution.md) | `pln-mqjykos6-ovw199` | fp-optics | Part V ch18 | done — `id_effect_optics` (maintenance) |
| [fp_state_process](../../.maestro/missions/fp-fsm.execution.md) | `pln-mqjykov9-bcblcg` | fp-fsm | Part V ch19 | done — `id_effect_fsm` reworked for 0.6 (runtime tier: real `#[derive(Fsm)]`, `FsmProcess`; session types removed) |
| [fp_parse_codec](../../.maestro/missions/fp-parse.execution.md) | `pln-mqjykox8-rmjgqz` | fp-parse | Part V ch20 | done — `id_effect_parse` moved to labs |
| [fp_runtime_resilience](../../.maestro/missions/fp-runtime.execution.md) | `pln-mqjykp1v-cdoxhr` | fp-runtime | Part V ch21 | done — `id_effect_resilience` (core), `retry_with_breaker` added in 0.6 |
| [fp_streaming_advanced](../../.maestro/missions/fp-streaming.execution.md) | `pln-mqjykp6a-uo9tbq` | fp-streaming | Part V ch22 | done — streams made pull-based in 0.6 |
| [fp_events_graph](../../.maestro/missions/fp-events.execution.md) | `pln-mqjykpbf-qbf6qg` | fp-events | Part V ch23 | done — `id_effect_events` / `id_effect_graph` moved to labs |
| [fp_dx_metaprogramming](../../.maestro/missions/fp-dx.execution.md) | `pln-mqjykpfk-a28tq5` | fp-dx | Part V ch24 | done — `derive(SchemaParser)` renamed `derive(Schema)` in 0.6 |

## Execution order

1. Algebra (bootstrap Part V)
2. Parse + Runtime (parallel)
3. Optics
4. FSM
5. Streaming
6. Events
7. DX

Meta orchestration: `pln-mqjykoig-a58dfk`
