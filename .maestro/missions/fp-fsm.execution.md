# Execution overlay: fp-fsm

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-fsm-crate-scaffold, leaf-fsm-core | yes | fp-optics wave 0, fp-runtime match macro |
| 1 | leaf-fsm-effect-interpreter, leaf-fsm-visualize, leaf-fsm-matcher-bridge | yes | 0 |
| 2 | leaf-saga-compensation, leaf-session-types | yes | 1 |
| 3 | leaf-fsm-workflow-bridge, leaf-fsm-book-skill | yes | 2 |
