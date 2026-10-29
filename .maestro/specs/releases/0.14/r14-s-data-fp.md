---
title: 'E13.1 — Sweep — accumulating Validation and NonEmpty collections in id_effect_fp: r14-s-data-fp'
slug: r14-s-data-fp
mode: heavy
work_type: spec-slice
risk_class: low
version: 1
acceptance_criteria:
  - 'data.fp#8 (row data.fp 75 → 100 with #7): new crates/id_effect_fp/src/collections/non_empty.rs with `NonEmptyVec<T>` (head plus tail, never empty by construction): `new(head)`, `from_vec(Vec<T>) -> Option<Self>`, `first` / `last` (no `Option`), `len() -> NonZeroUsize`, `push`, `map`, `iter`, `into_vec`, `IntoIterator`, `Semigroup` (concatenation; trait at crates/id_effect_fp/src/algebra/semigroup.rs:60), `Functor` and `Foldable` (crates/id_effect_fp/src/algebra/{functor,foldable}.rs), and a `non_empty_vec![..]` macro that rejects an empty invocation at compile time; plus `NonEmptyVector<T>` over the persistent `EffectVector` of crates/id_effect_fp/src/collections/vector.rs with the same surface. Nothing non-empty exists at HEAD (the only hit is a semigroup test name, semigroup.rs:500).'
  - 'data.fp#7: new crates/id_effect_fp/src/validation.rs with `Validation<A, E> { Valid(A), Invalid(NonEmptyVec<E>) }`: `valid`, `invalid`, `from_result`, `into_result -> Result<A, NonEmptyVec<E>>`, `map`, `map_err`, `zip` / `zip_with` / `map2`..`map4` and `ap` that collect the errors of both sides in order (applicative, never short-circuiting), `and_then` documented as the short-circuiting monadic step, `traverse_validation` / `sequence_validation` over an iterator; `Functor` and `Applicative` (crates/id_effect_fp/src/algebra/applicative.rs:48) impls; `Validation<A, E>: Semigroup` when `A: Semigroup`. Today only `traverse_vec` / `sequence_vec` exist (crates/id_effect_fp/src/lib.rs:62-65), which stop at the first error.'
  - 'Tests: crates/id_effect_fp/tests/validation.rs `zip_accumulates_both_errors_in_order`, `map3_collects_every_failing_field`, `ap_with_two_invalid_sides_keeps_left_then_right`, `traverse_validation_collects_every_error`, `into_result_round_trips_from_result`, `and_then_short_circuits`, `validation_satisfies_the_applicative_laws` (identity, homomorphism, interchange, composition, through crates/id_effect_fp/src/algebra/law_test.rs); crates/id_effect_fp/tests/non_empty.rs `from_vec_of_empty_is_none`, `first_and_last_never_fail`, `len_is_non_zero`, `semigroup_concatenates_in_order`, `non_empty_vector_matches_non_empty_vec`, `functor_and_foldable_laws_hold`; compile-fail doctest on `non_empty_vec![]`.'
  - 'Book: new chapter crates/id_effect/book/src/part5/ch17-04-validation-nonempty.md ("Validation and NonEmpty"), listed in crates/id_effect/book/src/SUMMARY.md under "Algebra type classes" after ch17-03 (append-only line). Examples: crates/id_effect_fp/examples/093_validation.rs (a form with three fields reporting every error), crates/id_effect_fp/examples/094_non_empty.rs (both auto-discovered, like 090-092).'
  - 'Tier: maintenance (id_effect_fp, ADR 0010: bar "frozen"). This leaf adds surface to a frozen crate, so it needs the scoped exception "id_effect_fp may add `Validation` and `NonEmpty*` in 0.14" in ADR 0010 Amendment 10 (written by r14-t0-skeletons) before it merges; the rest of the crate stays frozen. Secure by default: no new surface (pure data types, no I/O, no dependency added).'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_fp --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_fp --all-features`; `cargo test --doc -p id_effect_fp`; `cargo clippy -p id_effect_fp --all-targets --all-features -- -D warnings`; `bash scripts/tier-conformance.sh id_effect_fp`'
  - 'Owns: crates/id_effect_fp/src/validation.rs (new), crates/id_effect_fp/src/collections/non_empty.rs (new), crates/id_effect_fp/src/collections/mod.rs (module and re-export lines, append-only), crates/id_effect_fp/src/lib.rs (module and re-export lines, append-only), crates/id_effect_fp/tests/validation.rs (new), crates/id_effect_fp/tests/non_empty.rs (new), crates/id_effect_fp/examples/093_validation.rs (new), crates/id_effect_fp/examples/094_non_empty.rs (new), crates/id_effect/book/src/part5/ch17-04-validation-nonempty.md (new), crates/id_effect/book/src/SUMMARY.md (append-only), CHANGELOG.md (append-only)'
---

# r14-s-data-fp

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 — Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r14-t0-skeletons (ADR 0010 Amendment 10 must carry the id_effect_fp exception; see
the Tier criterion). Blocks: r14-p-ledger (promotes data.fp: Ledger must use `Validation` in a CI-run
test, D3), r14-x-level-audit. Shares no file with the other wave-1 leaves.

Defaults the owner may overturn: (1) the types go into id_effect_fp, where the scorecard evidence of
data.fp #1-#4 lives, under an Amendment 10 exception; the alternative is core
(`crates/id_effect/src/foundation/`, next to `either.rs`), which needs no exception but grows the slim
core. (2) `Validation` carries `NonEmptyVec<E>` rather than an `E: Semigroup`, so an `Invalid` always
has at least one error and no user `Semigroup` impl is needed. (3) No serde / `HasSchema` impls in this
leaf (a `NonEmptyVec` schema that rejects `[]` is a follow-up). Non-goals: `NonEmptyString`,
`NonEmptyMap`, a `Validation`-based schema decoder (core schema already accumulates `ParseErrors`).
