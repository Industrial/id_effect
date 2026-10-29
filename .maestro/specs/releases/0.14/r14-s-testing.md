---
title: 'E13.1 Sweep — first-party mocks with expectations, seeded test order: r14-s-testing'
slug: r14-s-testing
mode: heavy
work_type: spec-slice
risk_class: medium
version: 1
acceptance_criteria:
  - 'test.unit#4 (row test.unit 88 → 100, level production unchanged): a first-party mocking library in crates/id_effect_test, feature `mock` (no new dependency). `Mock<Args, Out>` holds ordered expectations: `expect()` then `.with(matcher)` (a predicate over the borrowed arguments), `.times(n)` / `.at_least(n)` / `.never()`, `.returning(f)` (any `Out`, an `Effect<A, E, R>` included) or `.return_once(v)`; `verify()` returns `Result<(), MockError>` naming each unmet expectation; an unexpected call panics with the call log (Debug of every call so far), which a `run_test` turns into `Cause::Die`; dropping a `Mock` with an unmet expectation panics unless the thread is already panicking. `mock! { pub MockUserRepo: UserRepo { fn find(&self, id: u64) -> Result<Option<User>, DbError>; … } }` (macro_rules, in `src/mock/macros.rs`) generates a struct implementing the capability trait with one `Mock` per method and `expect_<method>()`, so the double is provided exactly as the hand-written fakes of book ch15-03 are (`Users(Arc::new(mock))`, `Layer::succeed`, crates/id_effect/src/layer.rs:215). Today the row meets #2 with hand-written fakes (book part4/ch15-03-mocking.md "Layer-Based Test Setup") and #6 with scriptable doubles (`TestSqlClient`, `TestFileSystem`); there is no expectation library.'
  - 'test.determinism#8 (row test.determinism 88 → 100 once r14-p-ledger promotes the row, D3): test order randomised with a reproducible seed. crates/id_effect_test, feature `order`: `order::Suite` collects named tests and runs them through `libtest-mimic` (new dependency, exact-pinned) in an order shuffled by `id_effect::Random::seeded(seed)` (crates/id_effect/src/random.rs:219, Fisher-Yates); the seed comes from `ID_EFFECT_TEST_SEED`, else from entropy, and is printed first as `id_effect_test: test order seed S (rerun: ID_EFFECT_TEST_SEED=S)`; the same seed gives the same order and listing (`--list`, which nextest reads), so an order-dependent failure replays. Used from a `[[test]] harness = false` target. The existing seeded pieces (`TestExecutor::seeded` test_executor.rs:186, `explore_interleavings` :493) randomise fiber interleavings, not test order.'
  - 'Tests: crates/id_effect_test/tests/mock.rs `mock_returns_scripted_values_in_order`, `mock_matcher_selects_expectation_by_arguments`, `mock_unexpected_call_dies_with_call_log`, `mock_verify_reports_times_mismatch`, `mock_drop_panics_on_unmet_expectation`, `mock_macro_implements_capability_trait`, `mock_provided_as_layer_to_effect`, `mock_returning_effect_runs_on_test_executor`; crates/id_effect_test/tests/order.rs `same_seed_gives_same_order`, `different_seeds_give_different_orders`, `seed_read_from_env_and_reported`, `entropy_seed_is_reported_for_rerun`, `order_dependent_suite_fails_reproducibly_under_seed`, `listing_follows_the_seeded_order`; crates/id_effect_test/tests/order_harness.rs (harness = false) runs a small `Suite` under `cargo test` and nextest.'
  - 'Book: crates/id_effect/book/src/part4/ch15-03-mocking.md new section "Expectations with `mock!`" (the hand-written pattern stays first); new chapter crates/id_effect/book/src/part4/ch15-10-test-order.md (seeded order, rerun with the printed seed, why nextest''s process-per-test already isolates in-process state and what the harness adds: shared external state such as one database per binary). Examples crates/id_effect_test/examples/040_mocks.rs (feature `mock`) and 050_shuffled_order.rs (feature `order`). Both chapters compiled by examples/book-snippets (part4.rs).'
  - 'Tier: platform (id_effect_test, unchanged; ADR 0010 Amendment 5). Secure by default: no new surface (test-support code; nothing reachable from a non-test build unless an app depends on the features on purpose). New pre-1.0 dependency `libtest-mimic` exact-pinned, `cargo deny check` clean.'
  - 'Coverage: `cargo llvm-cov nextest -p id_effect_test --all-features --summary-only --fail-under-regions 96`'
  - 'Gate: `cargo nextest run -p id_effect_test --features mock,order`; `cargo test -p id_effect_test --features order --test order_harness`; `ID_EFFECT_TEST_SEED=42 cargo test -p id_effect_test --features order --test order_harness 2>&1 | grep -q "test order seed 42"`; `cargo clippy -p id_effect_test --all-targets --all-features -- -D warnings`; `cargo test -p book_snippets`; `bash scripts/tier-conformance.sh id_effect_test`; `cargo deny check`'
  - 'Owns: `crates/id_effect_test/src/mock/**` (new: `mod.rs`, `expectation.rs`, `macros.rs`), `crates/id_effect_test/src/order.rs` (new), `crates/id_effect_test/src/lib.rs` (the `mock` / `order` module lines and their crate-doc bullets; append-only), `crates/id_effect_test/Cargo.toml` (features `mock`, `order`, the `libtest-mimic` dependency, the `[[test]] order_harness` and `[[example]] 040_mocks` / `050_shuffled_order` entries), `crates/id_effect_test/tests/{mock,order,order_harness}.rs`, `crates/id_effect_test/examples/{040_mocks,050_shuffled_order}.rs`, `crates/id_effect_test/README.md` (feature list), `crates/id_effect/book/src/part4/ch15-03-mocking.md` (section "Expectations with `mock!`"), `crates/id_effect/book/src/part4/ch15-10-test-order.md`, append-only: `crates/id_effect/book/src/SUMMARY.md`, `examples/book-snippets/src/part4.rs` (include line), `examples/book-snippets/Cargo.toml` (`mock`, `order` in the id_effect_test features), `CHANGELOG.md`, `Cargo.lock`'
---

# r14-s-testing

Leaf of `.maestro/specs/releases/0.14.0.md`, section "E13.1 Sweep". The release spec's row is binding;
this file exists so Maestro can track the leaf as its own task.

Wave 1. Blocked by: r14-t0-skeletons (workspace and Cargo.lock settled). Blocks r14-p-ledger (the
Ledger runs one suite through `order::Suite` so `test.determinism` can be promoted, D3) and
r14-s-chaos (wave 3 edits the same `id_effect_test` lib.rs / Cargo.toml after this leaf merges).
`test.unit` is already production, so #4 counts as soon as it is published, tested, documented and has
its example; `test.determinism` stays documented (cap 89) until r14-p-ledger cites a CI test.

Defaults the owner may overturn: (1) mocks are a declarative `mock!` macro plus a builder, not a
`#[automock]` proc macro (no proc-macro crate, no edit to id_effect_proc_macro, core tier). If
macro_rules cannot express reference arguments (`&User`) cleanly, the fallback is matchers over
`(&A, &B)` tuples generated per arity up to 6, still declarative; a proc macro is a 0.15 decision.
(2) Seeded order is a libtest-mimic harness, not a nextest setting: nextest (0.9.x) has no
order-shuffle option and runs each test in its own process; re-check the nextest changelog at
EXECUTE and, if a shuffle option exists by then, document it in ch15-10 next to the harness.
