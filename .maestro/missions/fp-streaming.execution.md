# Execution overlay: fp-streaming

| Wave | Tasks | Parallel? | Blocked by |
|------|-------|-----------|------------|
| 0 | leaf-stream-window, leaf-stream-join | yes | fp-optics transducer |
| 1 | leaf-stream-hub-replay, leaf-stream-fsm | yes | 0 |
| 2 | leaf-stream-transducer | no | 1 |
| 3 | leaf-streaming-book-skill | no | 2 |
