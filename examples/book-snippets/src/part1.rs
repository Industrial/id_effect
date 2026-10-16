//! Book part 1: one `#[cfg(doctest)]` include per chapter file.

/// `part1/ch01-01-challenges-in-large-async-codebases.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch01-01-challenges-in-large-async-codebases.md")]
struct Ch0101ChallengesInLargeAsyncCodebases;

/// `part1/ch01-02-what-is-an-effect.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch01-02-what-is-an-effect.md")]
struct Ch0102WhatIsAnEffect;

/// `part1/ch01-03-type-parameters.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch01-03-type-parameters.md")]
struct Ch0103TypeParameters;

/// `part1/ch01-04-laziness.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch01-04-laziness.md")]
struct Ch0104Laziness;

/// `part1/ch02-01-creating-effects.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch02-01-creating-effects.md")]
struct Ch0201CreatingEffects;

/// `part1/ch02-02-transforming.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch02-02-transforming.md")]
struct Ch0202Transforming;

/// `part1/ch02-03-chaining.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch02-03-chaining.md")]
struct Ch0203Chaining;

/// `part1/ch02-04-real-program.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch02-04-real-program.md")]
struct Ch0204RealProgram;

/// `part1/ch03-01-why-do-notation.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch03-01-why-do-notation.md")]
struct Ch0301WhyDoNotation;

/// `part1/ch03-02-bind-operator.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch03-02-bind-operator.md")]
struct Ch0302BindOperator;

/// `part1/ch03-03-error-handling.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch03-03-error-handling.md")]
struct Ch0303ErrorHandling;

/// `part1/ch03-04-when-not-to-use.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part1/ch03-04-when-not-to-use.md")]
struct Ch0304WhenNotToUse;
