# Execution overlay: id-effect-rayon-default

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-spec-rayon-default | no | — |
| 1 | leaf-parallelism-core | no | 0 |
| 2 | leaf-collections-default-par | yes | 1 |
| 3 | leaf-stream-default-par | yes | 1 |
| 4 | leaf-api-migration-serial | no | 2–3 |
| 5 | leaf-docs-skill-examples | yes | 4 |
| 6 | leaf-verify-bench-ship | no | 5 |
