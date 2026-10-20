---
title: 'E2.5 — STM collections: r11-e2-5-stm-collections'
slug: r11-e2-5-stm-collections
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'conc.stm#6: TSet, TArray, TPriorityQueue (take retries when empty), TPromise (await_ parks until completed; completes once), TReentrantLock (read and write locks, reentrant per fiber) in id_effect::stm, each composing with Stm::or_else and Stm::retry.'
  - 'Tests `tset_add_remove_atomic_with_tref`, `tarray_update_is_transactional`, `tpriority_queue_orders_and_retries_when_empty`, `tpromise_await_parks_until_completed`, `treentrant_lock_writer_excludes_readers`, `stm_collections_contention`; examples 087-091; book part4/ch12-04-collections.md covers all nine structures (evidence for #6 at level documented, cap 89).'
  - 'Gate: `cargo nextest run -p id_effect --test stm_collections`; `cargo test -p id_effect --doc stm`; `cargo test -p id_effect --examples`'
  - 'Owns: `crates/id_effect/src/stm/{tset,tarray,tpriority_queue,tpromise,treentrant_lock}.rs`, `crates/id_effect/src/stm/mod.rs`, `crates/id_effect/examples/087-091_stm_*.rs`, `crates/id_effect/tests/stm_collections.rs`, `crates/id_effect/book/src/part4/ch12-04-collections.md`'
---

# r11-e2-5-stm-collections

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E2.5 — STM into core". The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Wave 2. Blocked by: r11-e2-5-stm-core.
