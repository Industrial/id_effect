# Execution overlay: fp-runtime

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-request-resolver, leaf-subscription-ref, leaf-match-macro | yes | — |
| 1 | leaf-redacted-schema, leaf-resilience-crate | yes | 0 |
| 2 | leaf-resilience-schedule, leaf-hedged-requests | yes | 1 |
| 3 | leaf-runtime-book-skill | no | 2 |
