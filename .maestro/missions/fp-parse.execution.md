# Execution overlay: fp-parse

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-parse-crate-scaffold, leaf-parser-combinator | yes | fp-algebra wave 1 |
| 1 | leaf-parse-effect-bridge, leaf-pretty-print | yes | 0 |
| 2 | leaf-invertible-codec, leaf-value-diff, leaf-parse-schema-bridge | yes | 1 |
| 3 | leaf-parse-book-skill | no | 2 |
