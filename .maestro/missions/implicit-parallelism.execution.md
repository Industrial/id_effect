# Execution overlay: implicit-parallelism

| Wave | Tasks (slug) | Parallel? | Blocked by |
|------|--------------|-----------|------------|
| 0 | leaf-adr-spec-harness | no | — |
| 1 | leaf-fabric-everywhere, leaf-fabric-aware-dispatch | yes | 0 |
| 2 | leaf-collection-api-collapse, leaf-stream-map-unify | yes | 1 |
| 3 | leaf-edg-independent-sets | no | 2 |
| 4 | leaf-remove-public-parallelism | no | 3 |
| 5 | leaf-docs-migration-040 | no | 4 |
