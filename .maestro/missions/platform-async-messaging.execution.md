# Execution overlay: platform-async-messaging

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-jobs-runner, leaf-outbox-pattern | yes | platform-data wave 1 |
| 1 | leaf-broker-kafka-adapter, leaf-events-sql-journal | yes | 0 |
| 2 | leaf-jobs-book-ch31 | no | 1 |
