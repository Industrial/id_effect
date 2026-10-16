//! Book cookbook and top-level pages (introduction, appendices): one `#[cfg(doctest)]` include per chapter file.
//!
//! `introduction.md` and appendices A, C and D have no Rust fences; appendix E is compiled by
//! `crates/id_effect/src/lib.rs`.

/// `cookbook/ch40-01-retry-test-clock.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/cookbook/ch40-01-retry-test-clock.md")]
struct Ch4001RetryTestClock;

/// `cookbook/ch40-02-bounded-subprocess-pool.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/cookbook/ch40-02-bounded-subprocess-pool.md")]
struct Ch4002BoundedSubprocessPool;

/// `cookbook/ch40-03-cli-run-main.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/cookbook/ch40-03-cli-run-main.md")]
struct Ch4003CliRunMain;

/// `cookbook/ch40-04-supervised-daemon.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/cookbook/ch40-04-supervised-daemon.md")]
struct Ch4004SupervisedDaemon;

/// `cookbook/ch40-05-streaming-etl.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/cookbook/ch40-05-streaming-etl.md")]
struct Ch4005StreamingEtl;

/// `appendix-b-migration.md`.
#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/appendix-b-migration.md")]
struct AppendixBMigration;
