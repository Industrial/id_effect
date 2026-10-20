//! Book part 7: one `#[cfg(doctest)]` include per chapter file.

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part7/ch37-00-processes-supervision.md")]
struct Ch3700ProcessesSupervision;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part7/ch38-00-distribution.md")]
struct Ch3800Distribution;

#[cfg(doctest)]
#[doc = include_str!("../../../crates/id_effect/book/src/part7/ch38-01-transport-security.md")]
struct Ch3801TransportSecurity;
