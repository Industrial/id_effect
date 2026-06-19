# Execution overlay: fp-optics

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-optics-crate-scaffold, leaf-lens-prism-optional | yes | fp-algebra wave 2 |
| 1 | leaf-optics-traverse, leaf-optics-schema-bridge | yes | 0 |
| 2 | leaf-optics-json-patch, leaf-zipper-trie | yes | 1 |
| 3 | leaf-optics-book-skill | no | 2 |
