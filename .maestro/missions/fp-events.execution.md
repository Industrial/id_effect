# Execution overlay: fp-events

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-event-store-capability, leaf-projection-runner | yes | fp-runtime subscription ref |
| 1 | leaf-cqrs-boundary, leaf-graph-dag-public | yes | 0 |
| 2 | leaf-events-schema | no | 1 |
| 3 | leaf-events-book-skill | no | 2 |
