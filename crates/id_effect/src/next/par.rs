//! Parallel combinators on fibers: `zip_par`, `all` and `for_each_par` under a
//! [`Concurrency`](crate::next::Concurrency) policy. Empty until `c-zip-par-all` fills it.
//!
//! A failure fails fast and interrupts the siblings; `Bounded(n)` caps the number in flight;
//! `for_each_par` keeps input order; `Sequential` behaves like a fold.
