# Execution overlay: platform-application

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-host-lifecycle, leaf-host-config-bootstrap | yes | platform-foundation wave 0 |
| 1 | leaf-auth-session, leaf-auth-oauth-trait | yes | 0 |
| 2 | leaf-security-middleware, leaf-host-book-ch30 | yes | 1 |
