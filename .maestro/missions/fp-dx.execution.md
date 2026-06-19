# Execution overlay: fp-dx

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-proptest-helpers, leaf-law-check-macros | yes | fp-algebra laws |
| 1 | leaf-cause-pretty, leaf-snapshot-expand | yes | 0 |
| 2 | leaf-derive-optics, leaf-derive-fsm, leaf-derive-schema-parse | yes | plans 02-04 |
| 3 | leaf-free-applicative, leaf-dx-book-skill | yes | 2 |
