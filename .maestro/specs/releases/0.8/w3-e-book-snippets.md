---
title: 0.8 W3-E — compile the book's Rust snippets (K5 >= 80 %)
slug: r08-w3-e-book-snippets
mode: light
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'Every chapter file under crates/id_effect/book/src that contains a rust fence is included in the examples/book-snippets harness (commit e7e7ac1: one module per part; #[cfg(doctest)] #[doc = include_str!(..)] struct per chapter), except the two already compiled by crates/id_effect/src/lib.rs (appendix-e-idiom-guide.md, part4/ch15-05-interleavings.md).'
  - 'At least 80 % of all rust fences in the book compile and run (or are no_run) via cargo test --doc -p book_snippets; snippets that describe code that no longer exists are corrected to the 0.8 API (the book must describe what the code does today); a snippet left rust,ignore has a one-line HTML comment reason directly above it.'
  - 'Hidden setup lines (# lines) keep the rendered snippet readable; mdbook build still succeeds (moon run :book or mdbook build crates/id_effect/book).'
non_goals:
  - Rewriting chapters beyond what compiling their snippets requires.
---

# 0.8 W3-E — book snippets

Split into three parallel slices by part (E1: part1, part2, cookbook + top level; E2: part3, part4; E3:
part5, part6, part7). Rows dx.docs #5/#6; KPI K5. Release spec `.maestro/specs/releases/0.8.0.md`.
