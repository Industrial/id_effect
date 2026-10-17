---
title: 'E4.4 — Storage, `id_effect_storage`: r10-e4-4-container-docs'
slug: r10-e4-4-container-docs
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'S3-compatible container pinned per D11; the conformance tests run against it; chapter + example.'
  - 'Gate: `<docker> -p id_effect_storage -p id_effect_test --all-features`; `bash scripts/tier-conformance.sh id_effect_storage`'
  - 'Owns: `crates/id_effect_test/src/containers/s3.rs` (+ one `mod` line), `crates/id_effect_storage/tests/s3.rs`, `book/src/part6/ch28-05-storage.md`, `crates/id_effect_storage/examples/010_storage.rs`'
---

# r10-e4-4-container-docs

Leaf of `.maestro/specs/releases/0.10.0.md`, section "E4.4 — Storage, `id_effect_storage` (M, wave 1) · web.storage #3 #5 #7 (+#1 #2 #8 kept)". The release spec's row is
binding; this file exists so Maestro can track the leaf as its own task.
