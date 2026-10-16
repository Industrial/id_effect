//! Book part 5: one `#[cfg(doctest)]` include per chapter file.

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch17-01-foldable-alternative.md")]
struct Ch1701FoldableAlternative;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch17-02-traversable.md")]
struct Ch1702Traversable;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch18-01-lenses.md")]
struct Ch1801Lenses;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch18-02-prisms-optionals.md")]
struct Ch1802PrismsOptionals;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch18-03-traversals-schema.md")]
struct Ch1803TraversalsSchema;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch19-01-transition-tables.md")]
struct Ch1901TransitionTables;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch19-02-effect-interpreter.md")]
struct Ch1902EffectInterpreter;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch19-03-saga-session.md")]
struct Ch1903SagaSession;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch20-00-parser-combinators.md")]
struct Ch2000ParserCombinators;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch21-00-runtime-resilience.md")]
struct Ch2100RuntimeResilience;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch21-01-resilience-pipelines.md")]
struct Ch2101ResiliencePipelines;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch22-01-windowing.md")]
struct Ch2201Windowing;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch22-02-joins.md")]
struct Ch2202Joins;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch22-03-replay-fanout.md")]
struct Ch2203ReplayFanout;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch22-04-state-scan.md")]
struct Ch2204StateScan;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch22-05-transducers.md")]
struct Ch2205Transducers;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch22-06-rate-and-grouping.md")]
struct Ch2206RateAndGrouping;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch23-00-events-and-projections.md")]
struct Ch2300EventsAndProjections;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part5/ch24-00-verification-and-macros.md")]
struct Ch2400VerificationAndMacros;
