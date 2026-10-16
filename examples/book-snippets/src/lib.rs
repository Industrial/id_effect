//! Compiles the `id_effect` book's Rust snippets (`crates/id_effect/book/src`) as doctests, so the
//! book cannot drift from the code (PLAN-OF-PLANS section 8, KPI K5).
//!
//! Each module includes the chapters of one book part with
//! `#[cfg(doctest)] #[doc = include_str!(..)] struct ..;`. A snippet that cannot compile stays
//! `rust,ignore` with a one-line reason comment above it. Run with
//! `cargo test --doc -p book_snippets`.

mod cookbook;
mod part1;
mod part2;
mod part3;
mod part4;
mod part5;
mod part6;
mod part7;
mod tutorial;
