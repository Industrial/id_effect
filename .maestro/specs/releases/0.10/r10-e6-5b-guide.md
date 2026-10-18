---
title: 'E6.5 part 2 — Fault injection: r10-e6-5b-guide'
slug: r10-e6-5b-guide
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Guide (#8) covering latency, seeded plans, partitions and kills (existing #3 #4); example; integration test.'
  - 'Gate: `cargo test --doc -p book_snippets`; same nextest'
  - 'Owns: `book/src/part4/ch15-09-fault-injection.md` + `SUMMARY.md` line, `crates/id_effect_test/examples/020_faults.rs`, `crates/id_effect_test/tests/faults.rs`'
---

# r10-e6-5b-guide

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E6.5 part 2 — Fault injection (M, wave 1) · test.chaos #2 #7 #8". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
