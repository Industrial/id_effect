---
title: 0.8 W3-D — top-20 mistake gallery with fixes; on_unimplemented sweep (E1.7, K4)
slug: r08-w3-d-diagnostics-gallery
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'crates/id_effect/tests/ui/gallery/: the 20 most likely mistakes when writing id_effect application code (missing capability, wrong provide, non-Send body, generic runner unannotated, missing retry factory, Layer output missing, effect! misuse, Never/Or error mismatches, block_on in async, etc.), each a trybuild fail case with a blessed .stderr on the pinned rustc (ui.rs BLESSED_RUSTC), registered in tests/ui.rs; the existing 9 fail + 4 pass cases still pass.'
  - 'K4 rule (owner decision 2026-10-16): a case counts when its first error carries a note (ours via #[diagnostic::on_unimplemented] note=, or rustc''s help) that states the concrete fix. Sweep the 7 existing on_unimplemented attributes (capability/hlist.rs, kernel/bind.rs, kernel/macro_support.rs, layer.rs) and proc-macro error texts so every gallery case meets the rule; all 20 meet it.'
  - 'docs/diagnostics.md (or a book appendix page; report the SUMMARY line) indexes each case: the mistake, the error, the fix.'
  - 'cargo test -p id_effect --test ui on the pinned rustc green; clippy -D warnings and docs green.'
non_goals:
  - New diagnostics machinery beyond on_unimplemented / proc-macro errors.
---

# 0.8 W3-D — diagnostics gallery

Rows dx.diagnostics (#4 fixes suggested); KPI K4. Release spec `.maestro/specs/releases/0.8.0.md`.
