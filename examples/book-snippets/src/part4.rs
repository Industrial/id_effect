//! Book part 4: one `#[cfg(doctest)]` include per chapter file.
//!
//! `part4/ch15-05-interleavings.md` is not included here: it is already a doctest of `id_effect`
//! (`crates/id_effect/src/lib.rs`).

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch12-00-stm.md")]
struct Ch1200Stm;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch12-01-why-stm.md")]
struct Ch1201WhyStm;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch12-02-tref.md")]
struct Ch1202Tref;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch12-03-stm-commit.md")]
struct Ch1203StmCommit;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch12-04-collections.md")]
struct Ch1204Collections;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch13-01-stream-vs-effect.md")]
struct Ch1301StreamVsEffect;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch13-02-chunks.md")]
struct Ch1302Chunks;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch13-03-backpressure.md")]
struct Ch1303Backpressure;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch13-04-sinks.md")]
struct Ch1304Sinks;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch13-05-parallelism.md")]
struct Ch1305Parallelism;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch14-01-unknown.md")]
struct Ch1401Unknown;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch14-02-combinators.md")]
struct Ch1402Combinators;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch14-03-validation.md")]
struct Ch1403Validation;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch14-04-parse-errors.md")]
struct Ch1404ParseErrors;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch15-01-run-test.md")]
struct Ch1501RunTest;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch15-02-test-clock.md")]
struct Ch1502TestClock;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch15-03-mocking.md")]
struct Ch1503Mocking;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch15-04-property-testing.md")]
struct Ch1504PropertyTesting;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part4/ch15-06-testing-property.md")]
struct Ch1506TestingProperty;
