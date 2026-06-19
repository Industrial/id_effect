# Execution overlay: fp-events (revised)

| Wave | Leaves | Parallel? | Blocked by |
|------|--------|-----------|------------|
| 0 | leaf-adr-es-entity-duroxide, leaf-fp-events-spec-revise | yes | platform-messaging PgPoolKey |
| 1 | leaf-events-es-entity-scaffold, leaf-workflow-duroxide-scaffold | yes | wave 0 |
| 2 | leaf-es-entity-event-facade, leaf-duroxide-pg-provider | yes | wave 1 |
| 3 | leaf-projection-runner-graph, leaf-cqrs-es-entity-bridge | yes | wave 2 |
| 4 | leaf-workflow-step-journal-duroxide, leaf-fsm-step-journal-generic | yes | wave 2 |
| 5 | leaf-e2e-events-workflow-example | no | waves 3–4 |
| 6 | leaf-events-book-skill, leaf-workflow-book-ch32-duroxide | yes | wave 5 |
| 7 | leaf-remove-legacy-pg-journal, leaf-graph-proptest | yes | wave 5 |

## Obsolete draft tasks

| Old slug | Status |
|----------|--------|
| leaf-graph-dag-public | shipped (id_effect_graph crate complete); scope in leaf-projection-runner-graph |
| leaf-event-store-capability | superseded by leaf-es-entity-event-facade |
| leaf-projection-runner | superseded by leaf-projection-runner-graph |
| leaf-cqrs-boundary | superseded by leaf-cqrs-es-entity-bridge |
| leaf-events-schema | folded into es-entity facade |
