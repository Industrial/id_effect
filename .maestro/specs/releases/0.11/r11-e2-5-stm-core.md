---
title: 'E2.5 — STM into core: r11-e2-5-stm-core'
slug: r11-e2-5-stm-core
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'id_effect::stm exposes Stm, TRef, TMap, TQueue, TSemaphore, Txn, Outcome, atomically and commit with unchanged semantics (moved from crates/id_effect_fp/src/stm/mod.rs, which depends only on id_effect::{Effect, from_async} at :38 and std); always compiled, no new dependencies (0.11.0.md D5); the global commit lock (:50) is unchanged.'
  - 'id_effect_fp::stm::* re-exports the core items with #[deprecated(note = "use id_effect::stm")] for one minor (L9); the re-export at crates/id_effect_fp/src/lib.rs:77 is kept and deprecated; the core crate doc (crates/id_effect/src/lib.rs:30-31) no longer says STM lives in id_effect_fp.'
  - 'Examples 077-086_stm_*.rs and tests/stm_contention.rs move to crates/id_effect and run there; book part4/ch12-00..03 point at id_effect::stm; MIGRATION-0.11 section "STM path".'
  - 'The kernel is unchanged: crates/id_effect/tests/kernel_alloc.rs and the core bench gates pass.'
  - 'Gate: `cargo nextest run -p id_effect -p id_effect_fp --all-features`; `cargo nextest run -p id_effect --test kernel_alloc`; `cargo test -p id_effect --examples`; `bash scripts/bench-compare.sh` (core gates)'
  - 'Owns: `crates/id_effect/src/stm/**`, `crates/id_effect/src/lib.rs`, `crates/id_effect/Cargo.toml`, `crates/id_effect/examples/077-086_stm_*.rs`, `crates/id_effect/tests/stm_contention.rs`, `crates/id_effect_fp/src/{stm/**,lib.rs}`, `crates/id_effect_fp/examples/077-086_stm_*.rs` (deleted), `crates/id_effect_fp/tests/stm_contention.rs` (deleted), `crates/id_effect/book/src/part4/ch12-0{0,1,2,3}-*.md`'
---

# r11-e2-5-stm-core

Leaf of `.maestro/specs/releases/0.11.0.md`, section "E2.5 — STM into core". The release spec's row
is binding; this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r11-t0-skeletons (Amendment 7 records the move). r11-e2-5-stm-collections
follows in wave 2.
